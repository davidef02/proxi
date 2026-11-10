import { lessonHref } from "../../router.ts";
import type { ChatItem } from "../../../../shared/protocol.ts";

type FinishItem = Extract<ChatItem, { kind: "finish" }>;
type NoticeItem = Extract<ChatItem, { kind: "notice" }>;

const link = "text-info underline underline-offset-2 hover:text-foreground";

export function FinishCard({ item }: { item: FinishItem }) {
  const r = item.result;
  return (
    <p className="font-prose text-sm text-muted-foreground">
      Lezione conclusa{r.anki ? `, ${r.anki}` : ""}.
      {r.lessonPath && (
        <>
          {" "}
          <a className={link} href={lessonHref(r.lessonPath)}>Rileggi la lezione</a>
        </>
      )}
    </p>
  );
}

export function Notice({ item }: { item: NoticeItem }) {
  const err = item.level === "error";
  return (
    <p role={err ? "alert" : "status"} className={`break-words text-center text-xs [overflow-wrap:anywhere] ${err ? "text-bad" : "text-muted-foreground"}`}>
      {item.text}
    </p>
  );
}
