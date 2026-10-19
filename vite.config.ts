import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { VitePWA } from "vite-plugin-pwa";

export default defineConfig({
  base: process.env.VITE_PAGES_BASE || "./",
  plugins: [
    react(),
    tailwindcss(),
    VitePWA({
      registerType: "autoUpdate",
      injectRegister: null,
      includeAssets: ["pwa-192.png", "pwa-512.png"],
      manifest: {
        name: "ZEROPASS",
        short_name: "ZEROPASS",
        description: "Private peer-to-peer transfer on your local network.",
        start_url: "./",
        scope: "./",
        display: "standalone",
        background_color: "#111513",
        theme_color: "#111513",
        icons: [
          { src: "pwa-192.png", sizes: "192x192", type: "image/png" },
          { src: "pwa-512.png", sizes: "512x512", type: "image/png" },
        ],
      },
      workbox: {
        clientsClaim: true,
        skipWaiting: true,
        globPatterns: ["**/*.{css,html,ico,js,png,svg,woff,woff2}"],
      },
    }),
  ],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});