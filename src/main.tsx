import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/500.css";
import "@fontsource/space-grotesk/400.css";
import "@fontsource/space-grotesk/500.css";
import "@fontsource/space-grotesk/600.css";
import "./styles.css";

import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import PwaApp from "./PwaApp";

const isTauri = "__TAURI_INTERNALS__" in window;

if (!isTauri && "serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    void navigator.serviceWorker.register("./sw.js").catch((error: unknown) => {
      console.error("PWA offline cache registration failed", error);
    });
  });
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    {isTauri ? <App /> : <PwaApp />}
  </React.StrictMode>,
);