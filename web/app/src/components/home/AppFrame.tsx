import type { ReactNode } from "react";
import { type Tab } from "@/lib/route";
import { cn } from "@/lib/utils";
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog";

const TABS: { tab: Tab; label: string; key: string; href: string }[] = [
  { tab: "home", label: "⌂ HOME", key: "1", href: "/" },
  { tab: "code", label: "CODE", key: "2", href: "/code" },
  { tab: "rag", label: "RAG", key: "3", href: "/rag" },
  { tab: "decisions", label: "DECISIONS", key: "4", href: "/decisions" },
];

const CHIP: Record<Tab, { label: string; className: string }> = {
  home: { label: "HOME", className: "bg-primary" },
  code: { label: "CODE", className: "bg-ok" },
  rag: { label: "RAG", className: "bg-info" },
  decisions: { label: "DECISIONS", className: "bg-special" },
};

const plain = (e: React.MouseEvent) => e.button === 0 && !e.metaKey && !e.ctrlKey && !e.shiftKey && !e.altKey;

export function TabBar({ tab, counts, onTab, onBoard, onHelp }: {
  tab: Tab;
  counts: { all: number; needs: number; working: number } | null;
  onTab: (tab: Tab) => void;
  onBoard: () => void;
  onHelp: () => void;
}) {
  return (
    <header className="flex h-[26px] shrink-0 items-center gap-2 overflow-x-auto border-b bg-bg0 px-2 font-mono text-[11px] whitespace-nowrap">
      <nav aria-label="Taburi" className="flex shrink-0 items-center gap-0.5">
        {TABS.map((t) => (
          <a key={t.tab} href={t.href} aria-current={tab === t.tab ? "page" : undefined} aria-keyshortcuts={t.key}
            onClick={(e) => { if (plain(e)) { e.preventDefault(); onTab(t.tab); } }}
            className={cn("inline-flex items-center gap-1 rounded-xs px-2 py-0.5 tracking-[0.04em] outline-none select-none focus-visible:ring-2 focus-visible:ring-ring",
              tab === t.tab ? "bg-sel2 font-bold text-foreground" : "text-dim hover:bg-bg2 hover:text-foreground")}>
            <span>{t.label}</span><span className={cn("text-[9px]", tab === t.tab ? "text-primary" : "text-dim")}>{t.key}</span>
          </a>
        ))}
      </nav>
      {counts && (
        <>
          <span aria-hidden className="text-dim">│</span>
          <span className="shrink-0 rounded-xs bg-bg1 px-1.5 text-[10.5px] text-muted-foreground">all <span className="tabular-nums">{counts.all}</span></span>
          <span className="shrink-0 text-err" aria-label={`${counts.needs} te așteaptă`}>● <span className="tabular-nums">{counts.needs}</span></span>
          <span className="shrink-0 text-warn" aria-label={`${counts.working} în lucru`}>● <span className="tabular-nums">{counts.working}</span></span>
        </>
      )}
      <a href="/board" onClick={(e) => { if (plain(e)) { e.preventDefault(); onBoard(); } }} aria-keyshortcuts="b"
        className="shrink-0 rounded-xs px-1.5 py-px text-[10px] text-dim outline-none hover:bg-bg2 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">
        b kanban
      </a>
      <span className="flex-1" />
      <button type="button" onClick={onHelp} aria-label="Taste" aria-keyshortcuts="?"
        className="shrink-0 rounded-xs px-1.5 py-px text-[10px] text-dim outline-none hover:bg-bg2 hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">?</button>
    </header>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="rounded-xs bg-bg2 px-[3px] font-mono text-[9.5px] text-foreground">{children}</kbd>;
}

export function ModeLine({ tab, hints, right }: { tab: Tab; hints: [string, string][]; right?: ReactNode }) {
  const chip = CHIP[tab];
  return (
    <footer className="hidden h-[22px] shrink-0 items-center gap-[7px] overflow-hidden border-t bg-bg1 pr-2 font-mono text-[10.5px] whitespace-nowrap text-dim md:flex">
      <span className={cn("inline-flex h-full shrink-0 items-center px-2 text-[10px] font-bold tracking-[0.05em] text-bg0", chip.className)}>{chip.label}</span>
      {hints.map(([key, label]) => (
        <span key={key} className="inline-flex shrink-0 items-center gap-[3px] px-0.5"><Kbd>{key}</Kbd><span>{label}</span></span>
      ))}
      <span className="ml-auto inline-flex shrink-0 items-center gap-[5px] text-[10px] text-muted-foreground">{right}</span>
    </footer>
  );
}

const KEYS: [string, [string, string][]][] = [
  ["Global", [["1 / 2 / 3 / 4", "HOME / CODE / RAG / DECISIONS"], ["c", "HOME ↔ CODE"], ["b", "kanban (/board)"], ["?", "tastele acestea"]]],
  ["HOME", [["j / k  ↑ / ↓", "cardul următor / anterior"], ["Enter", "deschide sesiunea cardului în CODE"], ["a", "aprobă sesiunea care așteaptă"], ["v", "primul artefact al cardului"]]],
];

export function KeysHelp({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="gap-3 font-mono sm:max-w-[560px]" aria-describedby={undefined}>
        <DialogTitle className="text-[11.5px] font-bold text-primary">? Taste</DialogTitle>
        {KEYS.map(([group, rows]) => (
          <section key={group} className="flex flex-col gap-1.5">
            <h3 className="text-[10px] text-dim uppercase">{group}</h3>
            <dl className="grid grid-cols-[110px_1fr] gap-x-2.5 gap-y-1 text-[10.5px] text-muted-foreground">
              {rows.map(([key, what]) => (
                <div key={key} className="contents"><dt><kbd className="rounded-xs border bg-bg2 px-[5px] text-foreground">{key}</kbd></dt><dd>{what}</dd></div>
              ))}
            </dl>
          </section>
        ))}
      </DialogContent>
    </Dialog>
  );
}
