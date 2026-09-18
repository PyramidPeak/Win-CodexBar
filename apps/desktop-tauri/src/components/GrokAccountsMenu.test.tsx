import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GrokAccount } from "../types/bridge";

const mocks = vi.hoisted(() => ({
  grokAccountsList: vi.fn(),
  grokAccountSwitch: vi.fn(),
  grokAccountFetch: vi.fn(),
}));
vi.mock("../lib/tauri", () => mocks);
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(() => Promise.resolve(() => {})) }));
vi.mock("../hooks/useLocale", () => ({ useLocale: () => ({ t: (key: string) => key }) }));
import GrokAccountsMenu from "./GrokAccountsMenu";

const first: GrokAccount = {
  id: "user-one",
  email: "one@example.com",
  organization: null,
  plan: "SuperGrok",
  isActive: true,
  isSaved: true,
};
const second: GrokAccount = {
  ...first,
  id: "user-two",
  email: "two@example.com",
  isActive: false,
};

describe("GrokAccountsMenu", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    mocks.grokAccountsList.mockResolvedValue([first, second]);
    mocks.grokAccountFetch.mockImplementation(async (id: string) => ({
      usedPercent: id === first.id ? 38 : 100,
      plan: "SuperGrok",
      windowMinutes: 10080,
      resetsAt: "2026-09-25T05:55:45Z",
    }));
  });

  it("shows Codex-style usage bars for each Grok account", async () => {
    const { container } = render(
      <GrokAccountsMenu hideEmail={false} resetTimeRelative />,
    );
    await screen.findByText(first.email);
    expect(screen.getAllByText("7d").length).toBeGreaterThan(0);
    expect(screen.getByText("38% PanelUsedSuffix")).toBeInTheDocument();
    expect(screen.getByText("100% PanelUsedSuffix")).toBeInTheDocument();
    const bars = container.querySelectorAll(".codex-menu-accounts__bar-fill");
    expect(bars).toHaveLength(2);
    expect((bars[0] as HTMLElement).style.width).toBe("38%");
    expect((bars[1] as HTMLElement).style.width).toBe("100%");
  });
});
