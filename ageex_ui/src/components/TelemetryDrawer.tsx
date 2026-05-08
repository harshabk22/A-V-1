import { useTelemetryStore } from "../stores/telemetryStore";

interface TelemetryDrawerProps {
  compactLayout?: boolean;
}

export default function TelemetryDrawer({
  compactLayout = false,
}: TelemetryDrawerProps) {
  const tick = useTelemetryStore((state) => state.tick);
  const clear = useTelemetryStore((state) => state.clear);
  const events = useTelemetryStore.getState().events;

  return (
    <aside
      style={{
        width: compactLayout ? "100%" : "clamp(280px, 24vw, 360px)",
        minWidth: compactLayout ? 0 : "280px",
        height: compactLayout ? "240px" : "100%",
        minHeight: 0,
        borderLeft: compactLayout ? "none" : "1px solid #273244",
        borderTop: compactLayout ? "1px solid #273244" : "none",
        background: "#0d1524",
        display: "flex",
        flexDirection: "column",
        overflow: "hidden",
      }}
    >
      <div
        style={{
          padding: "16px 18px",
          borderBottom: "1px solid #273244",
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          color: "#d9e8ff",
        }}
      >
        <div>
          <div style={{ fontWeight: 700 }}>Telemetry</div>
          <div style={{ fontSize: "0.78rem", color: "#8aa0bf" }}>
            Ring buffer {events.length}/200 · tick {tick}
          </div>
        </div>
        <button
          type="button"
          onClick={() => clear()}
          style={{
            border: "1px solid #39506f",
            background: "#132033",
            color: "#d9e8ff",
            borderRadius: "999px",
            padding: "6px 12px",
            cursor: "pointer",
          }}
        >
          Clear
        </button>
      </div>
      <div
        style={{
          flex: 1,
          overflowY: "auto",
          padding: "14px",
          display: "flex",
          flexDirection: "column",
          gap: "10px",
          overscrollBehavior: "contain",
        }}
      >
        {events.length === 0 ? (
          <div
            style={{
              color: "#8aa0bf",
              fontSize: "0.86rem",
              textAlign: "center",
              marginTop: "36px",
            }}
          >
            Waiting for kernel telemetry.
          </div>
        ) : (
          events.map((event) => (
            <div
              key={event.id}
              style={{
                border: "1px solid #26374f",
                borderRadius: "12px",
                background: "#101b2d",
                padding: "12px",
              }}
            >
              <div
                style={{
                  display: "flex",
                  justifyContent: "space-between",
                  gap: "8px",
                  marginBottom: "8px",
                  color: "#8aa0bf",
                  fontSize: "0.72rem",
                  textTransform: "uppercase",
                }}
              >
                <span>{event.kind}</span>
                <span>{new Date(event.createdAt).toLocaleTimeString()}</span>
              </div>
              <div
                style={{
                  color: "#f4f7fb",
                  fontFamily: "monospace",
                  fontSize: "0.82rem",
                  whiteSpace: "pre-wrap",
                  wordBreak: "break-word",
                }}
              >
                {event.summary}
              </div>
            </div>
          ))
        )}
      </div>
    </aside>
  );
}
