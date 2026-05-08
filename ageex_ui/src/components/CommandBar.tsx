import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

const CONTEXT_SCRAPED_EVENT_NAME = "context_scraped";
const SELECTED_TEXT_CHAR_CAP = 4000;
const TRUNCATED_SUFFIX = "...[TRUNCATED BY SYSTEM LIMIT]";

type ContextPayload = {
  active_app_name: string;
  active_window_title: string;
  selected_text?: string | null;
};

function previewContext(text: string): string {
  const compact = text.replace(/\s+/g, " ").trim();
  if (compact.length <= 90) {
    return compact;
  }
  return `${compact.slice(0, 87)}...`;
}

function capSelectedText(text?: string | null): string | null {
  if (!text) {
    return null;
  }
  if (text.length <= SELECTED_TEXT_CHAR_CAP) {
    return text;
  }
  return `${text.slice(0, SELECTED_TEXT_CHAR_CAP)}${TRUNCATED_SUFFIX}`;
}

export default function CommandBar() {
  const [input, setInput] = useState("");
  const [scrapedContext, setScrapedContext] = useState<ContextPayload | null>(null);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const previousHtmlBackground = document.documentElement.style.background;
    const previousBodyBackground = document.body.style.background;
    const previousBodyOverflow = document.body.style.overflow;
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.overflow = "hidden";

    let mounted = true;

    const unlistenPromise = listen<ContextPayload>(CONTEXT_SCRAPED_EVENT_NAME, (event) => {
      if (!mounted) {
        return;
      }
      const payload = event.payload;
      setScrapedContext(
        payload
          ? {
              ...payload,
              selected_text: capSelectedText(payload.selected_text),
            }
          : null,
      );
      setError(null);
      setInput("");
      window.requestAnimationFrame(() => {
        const active = document.getElementById("command-bar-input") as HTMLInputElement | null;
        active?.focus();
        active?.select();
      });
    });

    return () => {
      mounted = false;
      document.documentElement.style.background = previousHtmlBackground;
      document.body.style.background = previousBodyBackground;
      document.body.style.overflow = previousBodyOverflow;
      void unlistenPromise.then((unlisten) => unlisten());
    };
  }, []);

  const contextBadge = useMemo(() => {
    if (!scrapedContext) {
      return null;
    }
    const parts = [];
    if (scrapedContext.active_app_name || scrapedContext.active_window_title) {
      parts.push(
        `${scrapedContext.active_app_name || "Unknown App"}${
          scrapedContext.active_window_title ? ` · ${scrapedContext.active_window_title}` : ""
        }`,
      );
    }
    if (scrapedContext.selected_text) {
      parts.push(`"${previewContext(scrapedContext.selected_text)}"`);
    }
    if (parts.length === 0) {
      return null;
    }
    return `Context: ${parts.join(" — ")}`;
  }, [scrapedContext]);

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    const command = input.trim();
    if (!command || isSubmitting) {
      return;
    }

    const contextParts: string[] = [];
    if (scrapedContext?.active_app_name || scrapedContext?.active_window_title) {
      contextParts.push(
        `[App: ${scrapedContext?.active_app_name || "Unknown"}, Title: ${
          scrapedContext?.active_window_title || "Unknown"
        }]`,
      );
    }
    if (scrapedContext?.selected_text) {
      contextParts.push(capSelectedText(scrapedContext.selected_text) ?? "");
    }

    const task =
      contextParts.length > 0 ? `Context: ${contextParts.join("\n\n")}\n\nCommand: ${command}` : command;

    setIsSubmitting(true);
    setError(null);
    try {
      setInput("");
      setScrapedContext(null);
      const currentWindow = getCurrentWindow();
      const runPromise = invoke("execute_task", { task });
      await currentWindow.hide();
      await runPromise;
    } catch (submitError) {
      setError(String(submitError));
      await getCurrentWindow().show();
      await getCurrentWindow().setFocus();
    } finally {
      setIsSubmitting(false);
    }
  };

  const handleKeyDown = async (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Escape" && !isSubmitting) {
      event.preventDefault();
      setInput("");
      setError(null);
      await getCurrentWindow().hide();
    }
  };

  return (
    <div
      style={{
        minHeight: "100vh",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: "8px 12px",
        background: "transparent",
      }}
    >
      <form
        onSubmit={handleSubmit}
        style={{
          width: "min(700px, calc(100vw - 32px))",
          borderRadius: "20px",
          border: "1px solid rgba(124, 158, 212, 0.32)",
          background:
            "linear-gradient(180deg, rgba(10, 16, 26, 0.94), rgba(8, 12, 20, 0.92))",
          boxShadow: "0 24px 80px rgba(0, 0, 0, 0.42)",
          backdropFilter: "blur(18px)",
          padding: "10px 14px",
          display: "grid",
          gap: "8px",
        }}
      >
        {contextBadge ? (
          <div
            style={{
              color: "#cfe2ff",
              fontSize: "0.72rem",
              padding: "5px 9px",
              borderRadius: "999px",
              background: "rgba(35, 57, 92, 0.55)",
              border: "1px solid rgba(106, 138, 196, 0.32)",
              maxWidth: "100%",
              whiteSpace: "nowrap",
              overflow: "hidden",
              textOverflow: "ellipsis",
            }}
            title={
              scrapedContext?.selected_text ||
              [scrapedContext?.active_app_name, scrapedContext?.active_window_title]
                .filter(Boolean)
                .join(" · ")
            }
          >
            {contextBadge}
          </div>
        ) : null}
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: "10px",
          }}
        >
          <input
            id="command-bar-input"
            value={input}
            onChange={(event) => setInput(event.target.value)}
            onKeyDown={(event) => void handleKeyDown(event)}
            autoFocus
            placeholder="Ask MoatAgeex anything..."
            spellCheck={false}
            style={{
              flex: 1,
              border: "none",
              outline: "none",
              background: "transparent",
              color: "#f5f8ff",
              fontSize: "1.02rem",
              lineHeight: 1.3,
              padding: "2px 0",
            }}
          />
          <button
            type="submit"
            disabled={isSubmitting || !input.trim()}
            style={{
              border: "1px solid rgba(111, 181, 138, 0.42)",
              background: "rgba(21, 58, 34, 0.9)",
              color: "#d7ffe4",
              borderRadius: "999px",
              padding: "6px 12px",
              cursor: isSubmitting || !input.trim() ? "not-allowed" : "pointer",
              fontSize: "0.8rem",
              whiteSpace: "nowrap",
            }}
          >
            {isSubmitting ? "Running..." : "Run"}
          </button>
        </div>
        <div
          style={{
            color: error ? "#ffb4b4" : "#8ea0bc",
            fontSize: "0.72rem",
            minHeight: "1em",
          }}
        >
          {error ?? "Enter to run · Esc to dismiss"}
        </div>
      </form>
    </div>
  );
}
