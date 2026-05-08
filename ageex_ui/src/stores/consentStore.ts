import { create } from "zustand";
import type { ConsentRequest } from "../types/ipc";

interface ConsentState {
  queue: ConsentRequest[];
  isResolving: boolean;
  /** Push a new consent request to the back of the queue. */
  addRequest: (request: ConsentRequest) => void;
  /** Remove a resolved/denied request by its request_id. */
  removeRequest: (requestId: string) => void;
  /** Clear all pending requests (used during hard reset). */
  clearQueue: () => void;
  setResolving: (value: boolean) => void;
}

export const useConsentStore = create<ConsentState>((set) => ({
  queue: [],
  isResolving: false,
  addRequest: (request) =>
    set((state) => ({
      queue: [...state.queue, request],
      isResolving: false,
    })),
  removeRequest: (requestId) =>
    set((state) => ({
      queue: state.queue.filter((r) => r.request_id !== requestId),
      isResolving: false,
    })),
  clearQueue: () => set({ queue: [], isResolving: false }),
  setResolving: (value) => set({ isResolving: value }),
}));
