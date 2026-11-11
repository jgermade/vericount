// API endpoints for the AEAT sending: its state, the certificate and live mode.

import { request } from "../app/http"
import type { AeatQueue, Certificate } from "./types"

/// Base64 of a file's bytes, to send it in JSON.
async function base64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer())
  let binary = ""
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000))
  }
  return btoa(binary)
}

export const aeatApi = {
  aeatQueue: () => request<AeatQueue>("GET", "/aeat"),
  /// Uploads the AEAT certificate: a .p12/.pfx file with its password, or
  /// a PEM file with the certificate and its key.
  uploadCertificate: async (file: File, password: string) =>
    request<Certificate>("PUT", "/aeat/certificate", { file: await base64(file), password }),
  removeCertificate: () => request<null>("DELETE", "/aeat/certificate"),
  /// Turns live mode on, for good.
  goLive: () => request<AeatQueue>("POST", "/aeat/live", {}),
}
