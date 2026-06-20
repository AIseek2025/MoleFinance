// Buffer polyfill — MUST be imported before any module that touches
// `Buffer` at evaluation time (e.g. @solana/spl-token references it at the
// top level). ES module imports are evaluated in source order, so importing
// this module first guarantees the global is installed before the Solana /
// Borsh libraries in the App's import graph evaluate.
import { Buffer } from "buffer";

if (typeof globalThis.Buffer === "undefined") {
  (globalThis as unknown as { Buffer: typeof Buffer }).Buffer = Buffer;
}
