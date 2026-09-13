import { useState } from "react";
import { api, ApiError, setToken } from "@/lib/api";
import { Button } from "@/components/ui/button";

export function Login({ onAuthenticated }: { onAuthenticated: (projects: string[], host: string) => void }) {
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    const token = value.trim();
    if (token.length < 32 || busy) return;
    setBusy(true); setError(null);
    try {
      const data = await api<{ projects: string[]; host: string }>("projects", {}, undefined, { token });
      setToken(token);
      setValue("");
      onAuthenticated(data.projects, data.host);
    } catch (failure) {
      const problem = failure instanceof ApiError ? failure : null;
      setError(problem?.status === 401
        ? "Token respins. Folosește tokenul privat al gateway-ului Nebula (NEBULA_WEB_TOKEN), nu un token Cloudflare sau de model."
        : problem?.message ?? "Eroare neașteptată");
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="flex min-h-full flex-1 items-center justify-center px-5 py-10">
      <form onSubmit={(e) => { e.preventDefault(); void submit(); }} className="w-full max-w-sm">
        <p className="text-xs font-medium tracking-[0.2em] text-muted-foreground uppercase">Nebula</p>
        <h1 className="mt-3 text-3xl leading-tight font-semibold tracking-tight">Agenții continuă.<br />Tu alegi de unde.</h1>
        <p className="mt-3 text-sm text-muted-foreground">Sesiunile rulează pe server chiar și cu laptopul închis.</p>
        <label className="mt-8 block text-sm">
          <span className="font-medium">Token de acces</span>
          <input type="password" autoComplete="off" required minLength={32} value={value} onChange={(e) => setValue(e.target.value)}
            aria-describedby="token-help" aria-invalid={error ? true : undefined}
            className="mt-1.5 h-12 w-full rounded-lg border border-input bg-card px-3.5 text-base outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60" />
        </label>
        <p id="token-help" className="mt-2 text-xs text-muted-foreground">Rămâne doar în această sesiune de browser. Nu este trimis nicăieri altundeva.</p>
        {error && <p role="alert" className="mt-3 rounded-lg border border-border px-3 py-2 text-sm">{error}</p>}
        <Button type="submit" size="lg" className="mt-6 h-12 w-full text-base" disabled={busy || value.trim().length < 32}>
          {busy ? "Se conectează…" : "Conectare"}
        </Button>
      </form>
    </main>
  );
}
