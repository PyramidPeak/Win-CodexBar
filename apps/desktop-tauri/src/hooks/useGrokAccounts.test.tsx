import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const tauriMocks = vi.hoisted(() => ({
  grokAccountsList: vi.fn(),
  grokAccountFetch: vi.fn(),
}));

const eventMocks = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  listen: vi.fn(),
}));

vi.mock("../lib/tauri", () => tauriMocks);
vi.mock("@tauri-apps/api/event", () => eventMocks);

import { useGrokAccounts } from "./useGrokAccounts";

describe("useGrokAccounts", () => {
  it("treats a reload superseded by provider-updated as success", async () => {
    eventMocks.listen.mockImplementation(
      (name: string, handler: (event: { payload: unknown }) => void) => {
        eventMocks.listeners.set(name, handler);
        return Promise.resolve(() => {});
      },
    );
    tauriMocks.grokAccountsList.mockResolvedValue([]);
    const { result } = renderHook(() => useGrokAccounts());
    await waitFor(() => expect(tauriMocks.grokAccountsList).toHaveBeenCalled());

    // Hold the mutation's reload open until a provider-updated reload starts.
    let releaseList: (value: []) => void = () => {};
    tauriMocks.grokAccountsList.mockImplementationOnce(
      () => new Promise<[]>((resolve) => (releaseList = resolve)),
    );
    const onSuccess = vi.fn();
    let outcome: Promise<boolean> = Promise.resolve(false);
    await act(async () => {
      outcome = result.current.run(() => Promise.resolve(), onSuccess);
    });
    await act(async () => {
      eventMocks.listeners.get("provider-updated")?.({ payload: { providerId: "grok" } });
      releaseList([]);
    });

    await expect(outcome).resolves.toBe(true);
    expect(onSuccess).toHaveBeenCalledTimes(1);
  });
});
