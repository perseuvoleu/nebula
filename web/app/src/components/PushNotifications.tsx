import { useEffect, useState } from "react";
import { BellIcon, BellOffIcon } from "lucide-react";
import { api, describeError } from "@/lib/api";
import { applicationServerKey, pushSupported, unsubscribePush } from "@/lib/push";
import { Button } from "@/components/ui/button";

interface PushConfig { available: boolean; publicKey: string | null }

export function PushNotifications() {
  const [config, setConfig] = useState<PushConfig | null>(null);
  const [active, setActive] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api<PushConfig>("push").then(async (next) => {
      if (cancelled) return;
      setConfig(next);
      if (next.available && pushSupported()) setActive(Boolean(await (await navigator.serviceWorker.ready).pushManager.getSubscription()));
    }).catch(() => { if (!cancelled) setConfig({ available: false, publicKey: null }); });
    return () => { cancelled = true; };
  }, []);

  if (!config?.available) return null;
  if (!pushSupported()) return <p className="border-t border-sidebar-border px-3 py-2 text-xs text-muted-foreground">Notificările necesită aplicația instalată și HTTPS.</p>;

  const toggle = async () => {
    setBusy(true); setError(null);
    try {
      if (active) {
        await unsubscribePush();
        setActive(false);
      } else {
        if (await Notification.requestPermission() !== "granted") throw new Error("Permisiunea pentru notificări a fost refuzată.");
        const registration = await navigator.serviceWorker.ready;
        const subscription = await registration.pushManager.subscribe({
          userVisibleOnly: true,
          applicationServerKey: applicationServerKey(config.publicKey!),
        });
        try {
          await api("push/subscribe", {}, { subscription: subscription.toJSON() });
          setActive(true);
        } catch (failure) {
          await subscription.unsubscribe();
          throw failure;
        }
      }
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : describeError(failure));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="border-t border-sidebar-border px-3 py-2">
      <Button type="button" variant="outline" onClick={toggle} disabled={busy} className="w-full justify-start">
        {active ? <BellOffIcon aria-hidden /> : <BellIcon aria-hidden />}
        {busy ? "Se configurează…" : active ? "Dezactivează notificările" : "Activează notificările"}
      </Button>
      {error && <p role="alert" className="mt-2 text-xs text-destructive">{error}</p>}
    </div>
  );
}
