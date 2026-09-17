#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$RepoRoot = (Split-Path -Parent $PSScriptRoot),
    [double]$WarningFreeGiB = 60,
    [double]$HardFreeGiB = 35,
    [double]$WarningTargetGiB = 1,
    [double]$HardTargetGiB = 5,
    [double]$WarningTotalTargetGiB = 5,
    [double]$HardTotalTargetGiB = 10,
    [switch]$Json
)

function Get-NormalizedPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [string]$BasePath = ""
    )

    if (-not [System.IO.Path]::IsPathRooted($Path)) {
        $Path = Join-Path -Path $BasePath -ChildPath $Path
    }
    return ([System.IO.Path]::GetFullPath($Path)).TrimEnd('\')
}

function Test-PathWithin {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Candidate,
        [Parameter(Mandatory = $true)]
        [string]$Parent
    )

    $candidatePath = Get-NormalizedPath -Path $Candidate
    $parentPath = Get-NormalizedPath -Path $Parent
    return $candidatePath.Equals($parentPath, [System.StringComparison]::OrdinalIgnoreCase) -or
        $candidatePath.StartsWith($parentPath.TrimEnd('\') + '\', [System.StringComparison]::OrdinalIgnoreCase)
}

function Get-DirectoryBytes {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        return [int64]0
    }
    $sum = (Get-ChildItem -LiteralPath $Path -File -Recurse -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
    if ($null -eq $sum) {
        return [int64]0
    }
    return [int64]$sum
}

$ErrorActionPreference = 'Stop'
$repository = Get-NormalizedPath -Path $RepoRoot
$topLevel = (& git -C $repository rev-parse --show-toplevel 2>&1 | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($topLevel)) {
    throw "Not a Git worktree: $repository"
}
$repository = Get-NormalizedPath -Path $topLevel

$lines = @(git -C $repository worktree list --porcelain 2>&1)
if ($LASTEXITCODE -ne 0) {
    throw "Unable to enumerate Git worktrees for $repository"
}
$worktreePaths = @()
foreach ($line in $lines) {
    if ($line -like 'worktree *') {
        $path = $line.Substring(9).Trim()
        if (-not [string]::IsNullOrWhiteSpace($path)) {
            $worktreePaths += Get-NormalizedPath -Path $path -BasePath $repository
        }
    }
}

$worktrees = @()
foreach ($worktreePath in $worktreePaths) {
    $exists = Test-Path -LiteralPath $worktreePath -PathType Container
    $targetPath = Join-Path -Path $worktreePath -ChildPath 'target'
    $targetBytes = if ($exists) { Get-DirectoryBytes -Path $targetPath } else { [int64]0 }
    $nodeBytes = [int64]0
    if ($exists) {
        foreach ($nodePath in @(
            (Join-Path -Path $worktreePath -ChildPath 'node_modules'),
            (Join-Path -Path $worktreePath -ChildPath 'apps\desktop-tauri\node_modules')
        )) {
            $nodeBytes += Get-DirectoryBytes -Path $nodePath
        }
    }
    $dirty = $false
    if ($exists) {
        $dirty = @((git -C $worktreePath status --porcelain 2>$null)).Count -gt 0
    }
    $worktrees += [pscustomobject]@{
        Path = $worktreePath
        Exists = $exists
        Dirty = $dirty
        TargetBytes = $targetBytes
        NodeModulesBytes = $nodeBytes
    }
}

$driveRoot = [System.IO.Path]::GetPathRoot($repository)
$driveName = $driveRoot.TrimEnd('\').TrimEnd(':')
$drive = Get-PSDrive -Name $driveName -ErrorAction Stop
$freeBytes = [int64]$drive.Free
$targetBytesTotal = [int64](($worktrees | Measure-Object -Property TargetBytes -Sum).Sum)
$nodeBytesTotal = [int64](($worktrees | Measure-Object -Property NodeModulesBytes -Sum).Sum)
$sharedTarget = $env:CARGO_TARGET_DIR
$sharedTargetBytes = if (-not [string]::IsNullOrWhiteSpace($sharedTarget)) { Get-DirectoryBytes -Path $sharedTarget } else { [int64]0 }
$sharedInsideWorktree = $false
if (-not [string]::IsNullOrWhiteSpace($sharedTarget)) {
    foreach ($worktree in $worktrees) {
        if (Test-PathWithin -Candidate $sharedTarget -Parent $worktree.Path) {
            $sharedInsideWorktree = $true
            break
        }
    }
}

$warnings = @()
$hardFindings = @()
$freeGiB = $freeBytes / 1GB
$targetTotalGiB = $targetBytesTotal / 1GB
$nodeTotalGiB = $nodeBytesTotal / 1GB
$sharedTargetGiB = $sharedTargetBytes / 1GB
if ($freeGiB -lt $HardFreeGiB) { $hardFindings += "Free disk is $([math]::Round($freeGiB, 2)) GiB, below hard threshold $HardFreeGiB GiB." }
elseif ($freeGiB -lt $WarningFreeGiB) { $warnings += "Free disk is $([math]::Round($freeGiB, 2)) GiB, below warning threshold $WarningFreeGiB GiB." }
if ($targetTotalGiB -gt $HardTotalTargetGiB) { $hardFindings += "Registered worktree target total is $([math]::Round($targetTotalGiB, 2)) GiB, above hard threshold $HardTotalTargetGiB GiB." }
elseif ($targetTotalGiB -gt $WarningTotalTargetGiB) { $warnings += "Registered worktree target total is $([math]::Round($targetTotalGiB, 2)) GiB, above warning threshold $WarningTotalTargetGiB GiB." }
if ($sharedTargetGiB -gt 50) { $warnings += "Configured external Cargo target is $([math]::Round($sharedTargetGiB, 2)) GiB; inspect it before it grows further." }
if ($nodeTotalGiB -gt 5) { $warnings += "Registered worktree node_modules total is $([math]::Round($nodeTotalGiB, 2)) GiB." }
if ($worktrees.Count -gt 12) { $warnings += "There are $($worktrees.Count) registered worktrees; review inactive ones before creating more." }
foreach ($worktree in $worktrees) {
    $targetGiB = $worktree.TargetBytes / 1GB
    if ($targetGiB -gt $HardTargetGiB) { $hardFindings += "$($worktree.Path) has a $([math]::Round($targetGiB, 2)) GiB target directory." }
    elseif ($targetGiB -gt $WarningTargetGiB) { $warnings += "$($worktree.Path) has a $([math]::Round($targetGiB, 2)) GiB target directory." }
}
if ($sharedInsideWorktree) { $hardFindings += 'CARGO_TARGET_DIR points inside a registered worktree.' }

$result = [ordered]@{
    ReadOnly = $true
    Repository = $repository
    FreeGiB = [math]::Round($freeGiB, 2)
    RegisteredWorktrees = $worktrees.Count
    ExistingWorktrees = @($worktrees | Where-Object Exists).Count
    WorktreeTargetGiB = [math]::Round($targetTotalGiB, 2)
    WorktreeNodeModulesGiB = [math]::Round($nodeTotalGiB, 2)
    ConfiguredCargoTarget = $sharedTarget
    ConfiguredCargoTargetGiB = [math]::Round($sharedTargetGiB, 2)
    Warnings = $warnings
    HardFindings = $hardFindings
    Worktrees = $worktrees
}

if ($Json) {
    [pscustomobject]$result | ConvertTo-Json -Depth 6
} else {
    Write-Host "Worktree storage audit (read-only)" -ForegroundColor Cyan
    Write-Host "Repository: $repository"
    Write-Host "Free disk: $([math]::Round($freeGiB, 2)) GiB"
    Write-Host "Registered worktrees: $($worktrees.Count); existing: $(@($worktrees | Where-Object Exists).Count)"
    Write-Host "Worktree-local target: $([math]::Round($targetTotalGiB, 2)) GiB"
    Write-Host "Worktree node_modules: $([math]::Round($nodeTotalGiB, 2)) GiB"
    if (-not [string]::IsNullOrWhiteSpace($sharedTarget)) { Write-Host "Configured external Cargo target: $sharedTarget ($([math]::Round($sharedTargetGiB, 2)) GiB)" }
    foreach ($finding in $warnings) { Write-Warning $finding }
    foreach ($finding in $hardFindings) { Write-Warning "HARD: $finding" }
    if ($warnings.Count -eq 0 -and $hardFindings.Count -eq 0) { Write-Host 'No storage thresholds exceeded.' -ForegroundColor Green }
}

exit 0
