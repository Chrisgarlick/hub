// Passkeys in the browser (as in Board): turn Hub's JSON options into what
// navigator.credentials expects (ArrayBuffers), and the result back into
// base64url JSON for the server (crates/hub/src/passkeys.rs).

import { ApiError } from "./api.ts";

function toBytes(b64url: string): ArrayBuffer {
  const b64 = b64url.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(b64 + "=".repeat((4 - (b64.length % 4)) % 4));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out.buffer;
}

function toB64(buf: ArrayBuffer): string {
  let bin = "";
  for (const b of new Uint8Array(buf)) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function post<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method: "POST",
    credentials: "same-origin",
    headers: { "content-type": "application/json" },
    body: body === undefined ? null : JSON.stringify(body),
  });
  const data = (await res.json().catch(() => null)) as Record<string, unknown> | null;
  if (!res.ok) {
    throw new ApiError(
      res.status,
      String(data?.["error"] ?? "error"),
      String(data?.["message"] ?? `Hub said ${res.status}`),
    );
  }
  return data as T;
}

export const passkeysSupported = (): boolean =>
  typeof window !== "undefined" && "PublicKeyCredential" in window && !!navigator.credentials;

/** A friendly default name for a passkey made on this device. */
export function deviceName(): string {
  const ua = navigator.userAgent;
  if (/iPhone/.test(ua)) return "iPhone";
  if (/iPad/.test(ua)) return "iPad";
  if (/Android/.test(ua)) return "Android phone";
  if (/Macintosh/.test(ua)) return "Mac";
  if (/Windows/.test(ua)) return "Windows PC";
  return "This device";
}

interface CreationOptionsJson {
  challenge: string;
  rp: { id: string; name: string };
  user: { id: string; name: string; displayName: string };
  pubKeyCredParams: { type: "public-key"; alg: number }[];
  authenticatorSelection: AuthenticatorSelectionCriteria;
  attestation: AttestationConveyancePreference;
  excludeCredentials: { type: "public-key"; id: string }[];
  timeout: number;
}

interface RequestOptionsJson {
  challenge: string;
  rpId: string;
  userVerification: UserVerificationRequirement;
  timeout: number;
}

/** Make a passkey on this device (or a phone, via the browser's QR code) and register it. */
export async function addPasskey(name: string): Promise<void> {
  const o = await post<CreationOptionsJson>("/api/passkeys/options");
  const credential = (await navigator.credentials.create({
    publicKey: {
      ...o,
      challenge: toBytes(o.challenge),
      user: { ...o.user, id: toBytes(o.user.id) },
      excludeCredentials: o.excludeCredentials.map((c) => ({ ...c, id: toBytes(c.id) })),
    },
  })) as PublicKeyCredential | null;
  if (!credential) throw new Error("No passkey was made.");
  const r = credential.response as AuthenticatorAttestationResponse;
  await post("/api/passkeys", {
    name,
    clientDataJSON: toB64(r.clientDataJSON),
    attestationObject: toB64(r.attestationObject),
  });
}

/** Sign in with whichever Hub passkey the browser offers. */
export async function signInWithPasskey(): Promise<void> {
  const o = await post<RequestOptionsJson>("/auth/passkey/options");
  const credential = (await navigator.credentials.get({
    publicKey: { ...o, challenge: toBytes(o.challenge) },
  })) as PublicKeyCredential | null;
  if (!credential) throw new Error("No passkey was chosen.");
  const r = credential.response as AuthenticatorAssertionResponse;
  await post("/auth/passkey/login", {
    id: toB64(credential.rawId),
    clientDataJSON: toB64(r.clientDataJSON),
    authenticatorData: toB64(r.authenticatorData),
    signature: toB64(r.signature),
  });
}

/** What to tell someone when a passkey step fails. */
export function explain(err: unknown): string {
  if (err instanceof DOMException) {
    if (err.name === "NotAllowedError") return "Cancelled, or there's no passkey for Hub on this device.";
    if (err.name === "InvalidStateError") return "This device already has a passkey for Hub.";
    return err.message;
  }
  if (err instanceof ApiError) {
    return err.status === 401 ? "That passkey didn't work. Try again." : err.message;
  }
  return err instanceof Error ? err.message : String(err);
}
