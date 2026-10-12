export const CONSENT_EVENT_NAME = "require-consent";
export const KERNEL_READY_EVENT_NAME = "kernel-ready";
export const LIVE_TELEMETRY_EVENT_NAME = "live-telemetry";
export const GATEWAY_SERVER_EVENT_NAME = "gateway-server-event";
export const KERNEL_BOOTING_ERROR = "Kernel is still booting, please wait.";

export type ConsentDecision =
  | "approve_once"
  | "deny"
  | "create_permanent_policy";

export interface SubSpawnPolicyRequest {
  source_worker: string;
  target_worker: string;
  allowed_tool: string;
  can_create_permanent_policy: boolean;
}

export interface SkillReviewRequest {
  skill_name: string;
  description: string;
  source_code: string;
  code_hash: string;
  language: string;
  parameter_schema: string;
}

export interface ConsentRequest {
  request_id: string;
  session_id: string;
  action: string;
  summary: string;
  payload: string;
  summary_hash: string;
  payload_hash: string;
  rule_review?: RuleConflictReview;
  sub_spawn_policy?: SubSpawnPolicyRequest;
  skill_review?: SkillReviewRequest;
}

export interface LearnedRule {
  id: string;
  target_worker: string;
  rule_text: string;
  usage_count: number;
  confidence_score: number;
  expires_at: string;
}

export interface RuleConflictReview {
  new_rule: LearnedRule;
  conflicting_rules: LearnedRule[];
}

export interface KnowledgeMemoryRecord {
  id: string;
  kind: string;
  title: string;
  text: string;
  source: string;
  timestamp: number;
}

export interface MemoryStats {
  knowledgeCount: number;
  sessionCount: number;
  summarizedSessionCount: number;
  lastCompactedAt?: number | null;
}

export interface MemoryOverview {
  knowledge: KnowledgeMemoryRecord[];
  stats: MemoryStats;
}

export type SessionSource =
  | "desktop_ui"
  | "command_bar"

  | "automation"
  | "webchat"
  | "cli"
  | "whatsapp"
  | "discord"
  | "slack"
  | "telegram"
  | "signal"
  | "api"
  | "system";

export type SessionMessageRole = "user" | "agent" | "system" | "tool";

export interface GatewaySessionRecord {
  sessionId: string;
  source: SessionSource | string;
  channel?: string | null;
  title: string;
  latestSummary?: string | null;
  totalMessages: number;
  updatedAt: number;
}

export interface GatewayMessageRecord {
  sessionId: string;
  role: SessionMessageRole | string;
  content: string;
  timestamp: number;
}

export interface GatewaySessionContext {
  sessionId: string;
  source: SessionSource | string;
  channel?: string | null;
  title: string;
  latestSummary?: string | null;
  context: string;
  updatedAt: number;
}

export type GatewayServerEvent =
  | {
      kind: "adapter_registered";
      adapter_id: string;
      source: SessionSource | string;
      channel_id: string;
    }
  | {
      kind: "client_connected";
      client_id: string;
      role: string;
      source: SessionSource | string;
      channel_id: string;
    }
  | {
      kind: "client_disconnected";
      client_id: string;
    }
  | {
      kind: "session_resolved";
      client_id: string;
      session_id: string;
      source: SessionSource | string;
      channel_id: string;
    }
  | {
      kind: "session_created";
      session_id: string;
      source: SessionSource | string;
      channel_id?: string | null;
    }
  | {
      kind: "session_message_appended";
      session_id: string;
      role: SessionMessageRole | string;
      timestamp: number;
    }
  | {
      kind: "session_compacted";
      session_id: string;
      summary_chars: number;
    }
  | {
      kind: "request_status";
      request_id: string;
      message: string;
      timestamp: number;
    }
  | {
      kind: "token_stream";
      request_id?: string;
      session_id?: string;
      text: string;
    };

export interface AskChoiceTelemetry {
  type: "ask_choice";
  request_id: string;
  context: string;
  options: string[];
}

export interface StreamMessageTelemetry {
  agent_id: string;
  turn: number;
  content: string;
  is_partial: boolean;
  role?: "agent" | "system";
}

export interface ExecuteTaskResponse {
  text: string;
  totalTokens: number;
  sessionId?: string | null;
  role: "agent" | "system";
}

export interface BackgroundErrorEvent {
  kind: "background_error";
  severity: "warning" | "error" | "critical";
  subsystem: string;
  message: string;
  trace_id: string;
  timestamp: number;
  code?: string | null;
}

export interface GatewayClientDescriptor {
  label: string;
  device_id: string | null;
  capabilities: string[];
  connected_at: number;
}

export interface WorkerContextRequestEvent {
  kind: "worker_context_request";
  worker_id: string;
  task_id: string;
  question: string;
  blocking: boolean;
  timeout_secs: number;
}

export interface WorkerContextResponse {
  worker_id: string;
  task_id: string;
  answer: string;
}

export interface SettingsState {
  hasNvidia: boolean;
  hasDeepseek: boolean;
  chatModel?: string;
  plannerModel?: string;
  workerModel?: string;
}

export interface SettingsUpdate {
  chatModel?: string;
  plannerModel?: string;
  workerModel?: string;
  nvidiaApiKey?: string;
  deepseekApiKey?: string;
  provider?: string;
}

export function parseAskChoiceTelemetry(payload: string): AskChoiceTelemetry | null {
  try {
    const parsed = JSON.parse(payload) as Partial<AskChoiceTelemetry>;
    if (
      parsed.type === "ask_choice" &&
      typeof parsed.request_id === "string" &&
      typeof parsed.context === "string" &&
      Array.isArray(parsed.options) &&
      parsed.options.every((option) => typeof option === "string")
    ) {
      return {
        type: "ask_choice",
        request_id: parsed.request_id,
        context: parsed.context,
        options: parsed.options,
      };
    }
  } catch {
    return null;
  }
  return null;
}

export function parseBackgroundErrorTelemetry(
  payload: string,
): BackgroundErrorEvent | null {
  try {
    const parsed = JSON.parse(payload) as Partial<BackgroundErrorEvent>;
    if (
      parsed.kind === "background_error" &&
      (parsed.severity === "warning" ||
        parsed.severity === "error" ||
        parsed.severity === "critical") &&
      typeof parsed.subsystem === "string" &&
      typeof parsed.message === "string" &&
      typeof parsed.trace_id === "string" &&
      typeof parsed.timestamp === "number"
    ) {
      return {
        kind: "background_error",
        severity: parsed.severity,
        subsystem: parsed.subsystem,
        message: parsed.message,
        trace_id: parsed.trace_id,
        timestamp: parsed.timestamp,
        code: typeof parsed.code === "string" ? parsed.code : null,
      };
    }
  } catch {
    return null;
  }
  return null;
}

export function parseWorkerContextRequestTelemetry(
  payload: string,
): WorkerContextRequestEvent | null {
  try {
    const parsed = JSON.parse(payload) as Partial<WorkerContextRequestEvent>;
    if (
      parsed.kind === "worker_context_request" &&
      typeof parsed.worker_id === "string" &&
      typeof parsed.task_id === "string" &&
      typeof parsed.question === "string" &&
      typeof parsed.blocking === "boolean" &&
      typeof parsed.timeout_secs === "number"
    ) {
      return {
        kind: "worker_context_request",
        worker_id: parsed.worker_id,
        task_id: parsed.task_id,
        question: parsed.question,
        blocking: parsed.blocking,
        timeout_secs: parsed.timeout_secs,
      };
    }
  } catch {
    return null;
  }
  return null;
}

export const REASONING_ENVELOPE_EVENT = "reasoning-airgap-envelope";
export const REASONING_VIOLATION_EVENT = "reasoning-security-violation";
export const REASONING_YIELD_EVENT = "reasoning-agent-yield";
export const REASONING_RESPONSE_EVENT = "reasoning-airgap-response";

export interface AirgapResponseEvent {
  agentId: string;
  seq: number;
  op: string;
  isSuccess: boolean;
}

export type AgentState = "idle" | "running" | "yielded" | "trapped";

export interface AgentStatus {
  agentId: string;
  state: AgentState;
  fuelRemaining: number;
  fuelBudget: number;
  envelopesSent: number;
  yieldMessage: string;
}

export interface ReasoningStatus {
  agents: AgentStatus[];
  envelopeQueueSize: number;
  engineReady: boolean;
}

export interface StepOutcomeResponse {
  agentYielded: boolean;
  yieldMessage: string;
  totalRequestsSent: number;
  fuelUsed: number;
}

export interface AirgapEnvelopeEvent {
  agentId: string;
  seq: number;
  op: string;
  requestBytes?: number[];
}

export interface SecurityViolationEvent {
  agentId: string;
  seq: number;
  op: string;
  violation: string;
}

export interface AgentYieldEvent {
  agentId: string;
  yieldMessage: string;
  envelopesSent: number;
  fuelUsed: number;
}
