import { memo } from "react";
import type { Message } from "@/lib/types";
import { messageText } from "@/lib/conversation";
import { formatTime } from "@/lib/status";
import { CopyButton, Markdown } from "./Markdown";
import { ToolActivity } from "./ToolActivity";

export const MessageItem = memo(function MessageItem({ message, project, session }: {
  message: Message; project: string; session: string;
}) {
  const text = messageText(message);
  if (message.role === "user") {
    return (
      <article className="flex flex-col items-end gap-1" data-role="user" aria-label="Mesajul tău">
        <div className="max-w-[min(85%,72ch)] rounded-2xl rounded-br-md bg-white text-black px-4 py-2.5 text-[15px] leading-relaxed whitespace-pre-wrap break-words">
          {text || <span className="text-muted-foreground">[conținut fără text]</span>}
        </div>
        <time className="px-1 text-[11px] text-muted-foreground">{formatTime(message.timestamp)}</time>
      </article>
    );
  }
  if (message.role === "notice") {
    return (
      <aside className="mx-auto max-w-[72ch] rounded-lg border border-dashed border-border px-3 py-2 text-[13px] text-muted-foreground" data-role="notice">
        <span className="font-medium">{message.label === "compaction" ? "Context comprimat" : message.label === "branch" ? "Ramură abandonată" : message.label || "Notă"}</span>
        {text && <span> · {text.length > 400 ? `${text.slice(0, 400)}…` : text}</span>}
      </aside>
    );
  }
  const tools = message.parts.filter((p) => p.type === "tool");
  return (
    <article className="group flex flex-col gap-1" data-role={message.role} aria-label={message.role === "tool" ? "Activitate" : "Răspunsul agentului"}>
      {message.parts.map((part, index) => {
        if (part.type === "text") return <Markdown key={index} text={part.text + (part.truncated ? "\n\n…" : "")} />;
        if (part.type === "image") return <p key={index} className="text-sm text-muted-foreground">[imagine {part.mimeType}]</p>;
        return <ToolActivity key={part.callId || index} part={part} project={project} session={session} />;
      })}
      {message.error && <p role="alert" className="rounded-lg border border-border px-3 py-2 text-sm">Eroare model: {message.error}</p>}
      <div className="flex min-h-6 items-center gap-2 text-[11px] text-muted-foreground">
        <time>{formatTime(message.timestamp)}</time>
        {message.model && <span className="hidden sm:inline">{message.model}</span>}
        {tools.length > 0 && <span>{tools.length} {tools.length === 1 ? "unealtă" : "unelte"}</span>}
        {text && <CopyButton text={text} label="Copiază răspunsul" />}
      </div>
    </article>
  );
});
