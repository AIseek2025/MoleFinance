import React from "react";
import ReactDOM from "react-dom/client";
import { Buffer } from "buffer";
import { App } from "./App";
import "./styles.css";

// The frontend reuses Solana / Borsh helpers that expect Node's Buffer.
// Vite no longer injects it automatically, so expose the browser polyfill once.
if (typeof globalThis.Buffer === "undefined") {
  globalThis.Buffer = Buffer;
}

const rootEl = document.getElementById("root");
if (!rootEl) {
  throw new Error("MoleOption: missing #root element in index.html");
}

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);
