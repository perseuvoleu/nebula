import { api } from "./api.ts";

export function pushSupported(): boolean {
  return window.isSecureContext && "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
}

export function applicationServerKey(value: string): Uint8Array<ArrayBuffer> {
  const padded = value.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(value.length / 4) * 4, "=");
  return Uint8Array.from(atob(padded), (character) => character.charCodeAt(0));
}

export async function unsubscribePush(): Promise<void> {
  if (!pushSupported()) return;
  const subscription = await (await navigator.serviceWorker.ready).pushManager.getSubscription();
  if (!subscription) return;
  try {
    await api("push/unsubscribe", {}, { endpoint: subscription.endpoint }, { timeoutMs: 3000 });
  } finally {
    await subscription.unsubscribe();
  }
}
