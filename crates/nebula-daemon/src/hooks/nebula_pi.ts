// managed by nebula — rewritten on every pi agent spawn; edits are lost.
// Add your own extensions beside this file instead of editing it.
// Reports the session's busy/idle state to the nebula daemon and carries
// nebula's context injections (auto-title, notes) into the
// conversation. Inert outside nebula: without the NEBULA_* env vars it
// registers nothing.
// @ts-nocheck

export default function (pi) {
  const agentId = process.env.NEBULA_AGENT_ID;
  const apiUrl = process.env.NEBULA_API_URL;
  const token = process.env.NEBULA_API_TOKEN;
  if (!agentId || !apiUrl || !token) {
    return;
  }

  let sessionId;
  let rootSession = false;

  function updateSessionRef(ctx) {
    try {
      const id = ctx?.sessionManager?.getSessionId?.();
      if (typeof id === "string" && id.length > 0) {
        sessionId = id;
      }
    } catch {
      // keep the last known id
    }
  }

  async function post(event, extra) {
    const url =
      `${apiUrl}/api/hooks/pi?agentId=${encodeURIComponent(agentId)}` +
      `&hookEvent=${encodeURIComponent(event)}`;
    try {
      const res = await fetch(url, {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({
          session_id: sessionId,
          cwd: process.cwd(),
          ...extra,
        }),
        signal: AbortSignal.timeout(3000),
      });
      return await res.text();
    } catch {
      // The daemon being gone must never fault the user's turn.
      return "";
    }
  }

  pi.on("session_start", (event, ctx) => {
    // TUI only: print/RPC/JSON runs are headless — no nebula pane exists.
    if (ctx?.mode !== "tui") {
      return;
    }
    rootSession = true;
    updateSessionRef(ctx);
    void post("SessionStart", { source: event?.reason });
  });

  pi.on("before_agent_start", async (_event, ctx) => {
    if (!rootSession) {
      return;
    }
    updateSessionRef(ctx);
    // The daemon answers UserPromptSubmit with an (often empty) context
    // injection in the same envelope claude/codex hooks pipe to stdout.
    const body = await post("UserPromptSubmit");
    if (!body) {
      return;
    }
    try {
      const context = JSON.parse(body)?.hookSpecificOutput?.additionalContext;
      if (typeof context === "string" && context.length > 0) {
        return {
          message: {
            customType: "nebula-context",
            content: context,
            display: false,
          },
        };
      }
    } catch {
      // not an injection envelope — ignore
    }
  });

  pi.on("agent_settled", (_event, ctx) => {
    if (!rootSession || ctx?.isIdle?.() !== true) {
      return;
    }
    void post("Stop");
  });
}
