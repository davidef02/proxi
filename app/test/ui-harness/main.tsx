// Dev-only harness: renders SessionPage alone (inside a fake top bar slot) against the mock API of run.mjs. Not part of the shipped bundle.
import { useState } from "react";
import { createRoot } from "react-dom/client";
import "katex/dist/katex.min.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/500.css";
import "@fontsource/source-sans-3/400.css";
import "@fontsource/source-sans-3/400-italic.css";
import "@fontsource/source-sans-3/600.css";
import "./harness.css";
import { SessionPage } from "../../web/src/session/SessionPage.tsx";
import { TopBarSlotContext } from "../../web/src/shell/TopBarSlot.tsx";
import { APP_NAME } from "../../shared/identity.mjs";

function Harness() {
  const [el, setEl] = useState<HTMLElement | null>(null);
  return (
    <TopBarSlotContext.Provider value={{ el, count: 1, bump: () => {} }}>
      <header className="flex h-9 shrink-0 items-center gap-3 border-b border-hairline pl-4 pr-[140px] text-xs">
        <span className="font-medium">{APP_NAME}</span>
        <div ref={setEl} className="flex min-w-0 flex-1 items-center gap-3" />
      </header>
      <main className="flex min-h-0 flex-1 flex-col overflow-hidden">
        <SessionPage id={new URLSearchParams(location.search).get("s") ?? "transcript"} />
      </main>
    </TopBarSlotContext.Provider>
  );
}
createRoot(document.getElementById("root")!).render(<Harness />);
