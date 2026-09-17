#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$RepoRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$TargetRoot = $env:WCB_CARGO_TARGET_ROOT,
    [string]$TargetDirectory = $env:WCB_CARGO_TARGET_DIR
)

function Get-NormalizedPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [string]$BasePath = ""
    )

    if (-not [System.IO.Path]::IsPathRooted($Path)) {
        if ([string]::IsNullOrWhiteSpace($BasePath)) {
            throw "Path must be absolute: $Path"
        }
        $Path = Join-Path -Path $BasePath -ChildPath $Path
    }

    return ([System.IO.Path]::GetFullPath($Path)).TrimEnd('\')
}

function Get-PathHash {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $sha256 = [System.Security.Cryptography.SHA256]::Create()
    try {
        $bytes = [System.Text.Encoding]::UTF8.GetBytes($Path.ToLowerInvariant())
        $hash = $sha256.ComputeHash($bytes)
        return (($hash | ForEach-Object { $_.ToString('x2') }) -join '')
    } finally {
        $sha256.Dispose()
    }
}

function Test-PathWithin {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Candidate,
        [Parameter(Mandatory = $true)]
        [string]$Parent
    )

    $candidatePath = (Get-NormalizedPath -Path $Candidate)
    $parentPath = (Get-NormalizedPath -Path $Parent)
    return $candidatePath.Equals($parentPath, [System.StringComparison]::OrdinalIgnoreCase) -or
        $candidatePath.StartsWith($parentPath.TrimEnd('\') + '\', [System.StringComparison]::OrdinalIgnoreCase)
}

function Get-RegisteredWorktreePaths {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Repository
    )

    $lines = @(git -C $Repository worktree list --porcelain 2>&1)
    if ($LASTEXITCODE -ne 0) {
        throw "Unable to enumerate Git worktrees for $Repository"
    }

    $paths = @()
    foreach ($line in $lines) {
        if ($line -like 'worktree *') {
            $path = $line.Substring(9).Trim()
            if (-not [string]::IsNullOrWhiteSpace($path)) {
                $paths += Get-NormalizedPath -Path $path -BasePath $Repository
            }
        }
    }
    return $paths
}

function Set-WinCodexBarCargoTarget {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [string]$ExternalRoot,
        [string]$ExplicitTarget
    )

    $ErrorActionPreference = 'Stop'
    $repository = Get-NormalizedPath -Path $Root
    $topLevel = (& git -C $repository rev-parse --show-toplevel 2>&1 | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($topLevel)) {
        throw "Not a Git worktree: $repository"
    }
    $repository = Get-NormalizedPath -Path $topLevel

    $worktrees = @(Get-RegisteredWorktreePaths -Repository $repository)
    if ([string]::IsNullOrWhiteSpace($ExplicitTarget)) {
        if ([string]::IsNullOrWhiteSpace($ExternalRoot)) {
            $ExternalRoot = Join-Path -Path ([Environment]::GetFolderPath('LocalApplicationData')) -ChildPath 'Win-CodexBar\cargo-target'
        }
        $ExternalRoot = Get-NormalizedPath -Path $ExternalRoot

        $commonDir = (& git -C $repository rev-parse --git-common-dir 2>&1 | Out-String).Trim()
        if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($commonDir)) {
            throw "Unable to determine the Git common directory for $repository"
        }
        $commonDir = Get-NormalizedPath -Path $commonDir -BasePath $repository
        $repoId = (Get-PathHash -Path $commonDir).Substring(0, 16)
        $worktreeId = (Get-PathHash -Path $repository).Substring(0, 16)
        $target = Join-Path -Path (Join-Path -Path $ExternalRoot -ChildPath "repo-$repoId") -ChildPath "worktree-$worktreeId"
    } else {
        $target = Get-NormalizedPath -Path $ExplicitTarget
    }

    foreach ($worktree in $worktrees) {
        if (Test-PathWithin -Candidate $target -Parent $worktree) {
            throw "Refusing a Cargo target inside registered worktree: $target"
        }
    }

    $env:CARGO_TARGET_DIR = $target
    Write-Output "CARGO_TARGET_DIR=$target"
    Write-Output "CARGO_TARGET_SCOPE=process-local"
}

Set-WinCodexBarCargoTarget -Root $RepoRoot -ExternalRoot $TargetRoot -ExplicitTarget $TargetDirectory
