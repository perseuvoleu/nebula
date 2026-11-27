import { useEffect, useState } from 'react';
import { api } from '@/lib/api';
import { normalizeUsage, quotaLabel, startUsagePolling } from '@/lib/usage';
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogTitle, DialogTrigger } from '@/components/ui/dialog';
import { Button } from '@/components/ui/button';

const reasons: Record<string, string> = {
  account_unavailable: 'Contul CLI nu este autentificat pentru abonament sau accesul a fost refuzat.',
  credentials_expired: 'Autentificarea Claude a expirat. Reautentifică CLI-ul pe server.',
  cli_missing: 'CLI-ul Codex nu este disponibil pentru serviciul web.',
  missing_weekly: 'Furnizorul nu a returnat o cotă săptămânală pentru cont.',
  ambiguous_weekly: 'Există cote distincte pe modele; nu pot fi reunite într-un procent de cont.',
  invalid_data: 'Furnizorul a returnat date incomplete sau invalide.',
  timeout: 'Furnizorul nu a răspuns la timp.',
  rate_limited: 'Furnizorul a limitat cererile. Verificarea se reia automat după pauză.',
  expired_window: 'Ora de resetare a trecut; așteptăm o valoare nouă de la furnizor.',
  refresh_pending: 'Se așteaptă o verificare nouă.',
  provider_error: 'Cota nu poate fi verificată momentan.',
  disabled: 'Citirea cotelor este dezactivată în acest preview.',
  connection: 'Fără conexiune la server. Valoarea afișată poate fi veche.',
};
const formatDate = (seconds: number | null) => seconds === null ? 'necunoscută' : new Date(seconds * 1000).toLocaleString('ro-RO', { dateStyle: 'medium', timeStyle: 'short' });

/** Persistent account quota navbar, mounted once per authenticated app lifetime. */
export function UsageNavbar() {
  const [snapshot, setSnapshot] = useState<unknown>({ codex: { remainingPercent: null, status: 'loading' }, claude: { remainingPercent: null, status: 'loading' } });
  const [now, setNow] = useState(() => Date.now() / 1000);
  useEffect(() => {
    const stop = startUsagePolling(
      signal => api<unknown>('usage', {}, undefined, { signal, timeoutMs: 20000 }),
      value => {
        setNow(Date.now() / 1000);
        setSnapshot((old: unknown) => {
          if (!(value instanceof Error)) return value;
          const previous = normalizeUsage(old, Date.now() / 1000);
          return Object.fromEntries(Object.entries(previous).map(([key, quota]) => [key, {
            ...quota, status: quota.remainingPercent === null ? 'unavailable' : 'stale', reason: 'connection',
          }]));
        });
      }, { document, window, navigator });
    // Age displayed values independently from requests; never manufacture reset recovery.
    const timer = setInterval(() => { if (!document.hidden) setNow(Date.now() / 1000); }, 30000);
    return () => { stop(); clearInterval(timer); };
  }, []);
  const data = normalizeUsage(snapshot, now);
  return (
    <header className="shrink-0 border-b bg-bg0 px-2" aria-label="Cote săptămânale de cont">
      <Dialog>
        <DialogTrigger asChild>
          <button type="button" className="mx-auto flex min-h-9 w-full max-w-lg items-center justify-center gap-2 rounded-sm px-2 py-1 text-xs hover:bg-bg1 focus-visible:outline-2 focus-visible:outline-ring" aria-label="Detalii cote săptămânale Codex și Claude">
            <span className="label-caps">Săptămână rămase</span>
            <span className="rounded-full border bg-bg1 px-2 py-0.5 font-mono text-[11px] text-muted-foreground tabular-nums">{quotaLabel('Codex', data.codex)}</span>
            <span className="rounded-full border bg-bg1 px-2 py-0.5 font-mono text-[11px] text-muted-foreground tabular-nums">{quotaLabel('Claude', data.claude)}</span>
          </button>
        </DialogTrigger>
        <DialogContent showCloseButton={false} className="max-h-[85dvh] overflow-y-auto">
          <DialogTitle>Cote săptămânale</DialogTitle>
          <DialogDescription>Abonamentele conturilor de pe server. Ora resetării este afișată în fusul tău orar.</DialogDescription>
          {(['codex', 'claude'] as const).map(provider => {
            const quota = data[provider];
            return <section key={provider} className="space-y-1 rounded-md border bg-bg0 p-3">
              <h2 className="font-medium">{quotaLabel(provider === 'codex' ? 'Codex' : 'Claude', quota)}{quota.remainingPercent !== null && ' rămase'}</h2>
              <p className="text-xs text-muted-foreground">{quota.status === 'loading' ? 'Se verifică…' : quota.status === 'unavailable' ? 'Indisponibil' : quota.status === 'stale' ? 'Valoare veche — neconfirmată' : 'Verificat'}</p>
              <p className="text-xs">Resetare: <span className="font-mono text-muted-foreground">{formatDate(quota.resetAt)}</span></p>
              <p className="text-xs">Ultima verificare reușită: <span className="font-mono text-muted-foreground">{formatDate(quota.updatedAt)}</span></p>
              {quota.reason && <p className="text-xs text-muted-foreground">{reasons[quota.reason] ?? reasons.provider_error}</p>}
            </section>;
          })}
          <p className="text-xs text-muted-foreground">Verificare la 5 minute, cât timp aplicația este vizibilă și online. Claude folosește un endpoint OAuth nedocumentat, care poate deveni indisponibil.</p>
          <DialogClose asChild><Button variant="outline" className="min-h-11">Închide</Button></DialogClose>
        </DialogContent>
      </Dialog>
    </header>
  );
}
