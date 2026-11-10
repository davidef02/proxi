import { useEffect, useRef } from "react";
import { FileImage, FileText } from "lucide-react";
import type { ChatItem } from "../../../shared/protocol.ts";
import { Markdown } from "./Markdown.tsx";
import { ToolChip } from "./ToolChip.tsx";
import { QuizCard } from "./cards/QuizCard.tsx";
import { AskCard } from "./cards/AskCard.tsx";
import { PlanCard } from "./cards/PlanCard.tsx";
import { CurriculumCard } from "./cards/CurriculumCard.tsx";
import { NodeCard } from "./cards/NodeCard.tsx";
import { ExerciseCard } from "./cards/ExerciseCard.tsx";
import { ProblemCard } from "./cards/ProblemCard.tsx";
import { FinishCard, Notice } from "./cards/FinishCard.tsx";
import { JobCard } from "./cards/JobCard.tsx";

interface Ctx {
  sessionId: string;
  live: boolean;
  /** requestId of the item shown in the dock: quizzes and asks render nothing here until they are answered. */
  pendingId?: string;
}

/** Items that draw nothing (empty teacher text, a quiz/ask that is still in the dock). */
const hidden = (it: ChatItem, ctx: Ctx) =>
  (it.kind === "teacher" && !it.text && !it.streaming) || ((it.kind === "quiz" || it.kind === "ask") && it.requestId === ctx.pendingId);

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function Item({ item, ctx }: { item: ChatItem; ctx: Ctx }) {
  switch (item.kind) {
    case "learner":
      return (
        <div className="flex flex-col items-end gap-1.5">
          {item.attachments && item.attachments.length > 0 && (
            <div className="flex max-w-[80%] flex-wrap justify-end gap-1.5">
              {item.attachments.map((att) => {
                const isImg = att.mimeType.startsWith("image/") || /\.(png|jpe?g|webp|gif|svg)$/i.test(att.name);
                const Icon = isImg ? FileImage : FileText;
                return (
                  <span
                    key={att.id}
                    className="inline-flex items-center gap-1.5 rounded-md border border-border bg-card/80 px-2 py-1 text-xs text-foreground shadow-sm"
                    title={att.path}
                  >
                    <Icon className="h-3.5 w-3.5 text-muted-foreground" />
                    <span className="font-medium">{att.name}</span>
                    <span className="text-[10px] text-muted-foreground">{formatBytes(att.size)}</span>
                  </span>
                );
              })}
            </div>
          )}
          <p className="max-w-[80%] whitespace-pre-wrap break-words rounded-lg bg-accent px-3.5 py-2 font-prose text-[15px] leading-relaxed [overflow-wrap:anywhere]">{item.text}</p>
        </div>
      );
    case "teacher":
      return (
        <div className="min-w-0 break-words [overflow-wrap:anywhere] [&_pre]:overflow-x-auto [&_pre]:[overflow-wrap:normal]">
          <Markdown text={item.text} streaming={item.streaming && ctx.live} />
        </div>
      );
    case "tool":
      return <ToolChip item={item} />;
    case "quiz":
      return <QuizCard item={item} />;
    case "ask":
      return <AskCard item={item} />;
    case "plan":
      return <PlanCard item={item} />;
    case "curriculum":
      return <CurriculumCard item={item} />;
    case "node":
      return <NodeCard item={item} />;
    case "exercise":
      return <ExerciseCard exercise={item.exercise} sessionId={ctx.sessionId} />;
    case "problem":
      return <ProblemCard item={item} />;
    case "finish":
      return <FinishCard item={item} />;
    case "notice":
      return <Notice item={item} />;
    case "job":
      return <JobCard job={item.job} />;
  }
}

export function Chat({ items, sessionId, live, pendingId }: { items: ChatItem[] } & Ctx) {
  const box = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const ctx: Ctx = { sessionId, live, pendingId };
  const visible = items.filter((it) => !hidden(it, ctx));
  // Follow the stream only while the learner has not scrolled up.
  useEffect(() => {
    const el = box.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [items]);
  // Dock growth / late layout (images, KaTeX, iframes) changes sizes without changing items: re-stick while at the bottom.
  useEffect(() => {
    const el = box.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    const ro = new ResizeObserver(() => {
      if (stick.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(el);
    if (el.firstElementChild) ro.observe(el.firstElementChild);
    return () => ro.disconnect();
  }, []);
  return (
    <div
      ref={box}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
      }}
      data-chat-scroll
      className="relative min-h-[140px] min-w-0 flex-1 overflow-x-hidden overflow-y-auto"
      role="log"
      aria-label="Conversazione"
    >
      <div className="mx-auto w-full max-w-[46rem] space-y-5 px-4 py-6">
        {visible.length === 0 && (
          <p className="pt-10 text-center font-prose text-[15px] text-muted-foreground">
            {pendingId ? "Prima di cominciare, una domanda." : "Il docente sta preparando l’inizio della sessione…"}
          </p>
        )}
        {visible.map((it) => (
          <Item key={it.id} item={it} ctx={ctx} />
        ))}
      </div>
    </div>
  );
}
