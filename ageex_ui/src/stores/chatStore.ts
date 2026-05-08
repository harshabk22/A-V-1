import { create } from "zustand";
import type { KnowledgeMemoryRecord, StreamMessageTelemetry } from "../types/ipc";

export interface ChatMessage {
  id: string;
  role: "user" | "agent" | "system";
  text: string;
  createdAt: number;
  choiceOptions?: string[];
  choiceResolved?: boolean;
  selectedChoice?: string;
  /** Backend request_id for resolving user:ask_choice via IPC */
  choiceRequestId?: string;
  isStreaming?: boolean;
}

interface ChatState {
  messages: ChatMessage[];
  messageIndex: Record<string, number>;
  input: string;
  isBooting: boolean;
  isProcessing: boolean;
  statusText: string;
  sessionTokens: number;
  showMemory: boolean;
  knowledgeEntries: KnowledgeMemoryRecord[];
  setInput: (value: string) => void;
  setBooting: (value: boolean) => void;
  setProcessing: (value: boolean) => void;
  setStatusText: (value: string) => void;
  setSessionTokens: (value: number) => void;
  addSessionTokens: (value: number) => void;
  setShowMemory: (value: boolean) => void;
  setKnowledgeEntries: (knowledgeEntries: KnowledgeMemoryRecord[]) => void;
  addUserMessage: (text: string) => void;
  addAgentMessage: (text: string) => void;
  addSystemMessage: (text: string) => void;
  addChoiceMessage: (context: string, options: string[], requestId: string) => string;
  resolveChoice: (id: string, selectedChoice: string) => void;
  upsertStreamingMessage: (payload: StreamMessageTelemetry) => void;
  clearConversation: () => void;
}

function makeMessageId(prefix: string): string {
  return `${prefix}:${Date.now()}:${Math.random().toString(36).slice(2, 8)}`;
}

function mergeStreamingText(previous: string, incoming: string): string {
  if (!previous) {
    return incoming;
  }
  if (incoming.startsWith(previous)) {
    return incoming;
  }
  if (previous.endsWith(incoming)) {
    return previous;
  }
  return `${previous}${incoming}`;
}

function streamMessageKey(payload: StreamMessageTelemetry): string {
  return `stream:${payload.agent_id}:${payload.turn}`;
}

export const useChatStore = create<ChatState>((set) => ({
  messages: [],
  messageIndex: {},
  input: "",
  isBooting: true,
  isProcessing: false,
  statusText: "◌ BOOTING KERNEL",
  sessionTokens: 0,
  showMemory: false,
  knowledgeEntries: [],
  setInput: (value) => set({ input: value }),
  setBooting: (value) => set({ isBooting: value }),
  setProcessing: (value) => set({ isProcessing: value }),
  setStatusText: (value) => set({ statusText: value }),
  setSessionTokens: (value) => set({ sessionTokens: value }),
  addSessionTokens: (value) =>
    set((state) => ({ sessionTokens: state.sessionTokens + value })),
  setShowMemory: (value) => set({ showMemory: value }),
  setKnowledgeEntries: (knowledgeEntries) => set({ knowledgeEntries }),
  addUserMessage: (text) =>
    set((state) => ({
      messages: [
        ...state.messages,
        {
          id: makeMessageId("user"),
          role: "user",
          text,
          createdAt: Date.now(),
        },
      ],
    })),
  addAgentMessage: (text) =>
    set((state) => ({
      messages: [
        ...state.messages,
        {
          id: makeMessageId("agent"),
          role: "agent",
          text,
          createdAt: Date.now(),
        },
      ],
    })),
  addSystemMessage: (text) =>
    set((state) => ({
      messages: [
        ...state.messages,
        {
          id: makeMessageId("system"),
          role: "system",
          text,
          createdAt: Date.now(),
        },
      ],
    })),
  addChoiceMessage: (context, options, requestId) => {
    const id = makeMessageId("choice");
    set((state) => ({
      messages: [
        ...state.messages,
        {
          id,
          role: "agent",
          text: context,
          createdAt: Date.now(),
          choiceOptions: options,
          choiceResolved: false,
          choiceRequestId: requestId,
        },
      ],
    }));
    return id;
  },
  resolveChoice: (id, selectedChoice) =>
    set((state) => {
      const index = state.messages.findIndex((message) => message.id === id);
      if (index < 0) {
        return state;
      }
      const messages = state.messages.slice();
      messages[index] = {
        ...messages[index],
        choiceResolved: true,
        selectedChoice,
      };
      return { messages };
    }),
  upsertStreamingMessage: (payload) =>
    set((state) => {
      const key = streamMessageKey(payload);
      const currentIndex = state.messageIndex[key];
      const role = payload.role === "system" ? "system" : "agent";

      if (currentIndex === undefined) {
        const newIndex = state.messages.length;
        return {
          messages: [
            ...state.messages,
            {
              id: makeMessageId("stream"),
              role,
              text: payload.content,
              createdAt: Date.now(),
              isStreaming: payload.is_partial,
            },
          ],
          messageIndex: {
            ...state.messageIndex,
            [key]: newIndex,
          },
        };
      }

      const messages = state.messages.slice();
      const existing = messages[currentIndex];
      messages[currentIndex] = {
        ...existing,
        role,
        text: mergeStreamingText(existing.text, payload.content),
        isStreaming: payload.is_partial,
      };

      if (payload.is_partial) {
        return { messages };
      }

      const messageIndex = { ...state.messageIndex };
      delete messageIndex[key];
      return { messages, messageIndex };
    }),
  clearConversation: () =>
    set({
      messages: [],
      messageIndex: {},
      input: "",
      isProcessing: false,
      sessionTokens: 0,
      showMemory: false,
      knowledgeEntries: [],
      statusText: "○ STANDBY",
    }),
}));
