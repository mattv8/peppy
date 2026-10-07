import { resolve } from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: [{ find: "./bridge", replacement: resolve(__dirname, "src/ui-bridge.ts") }],
  },
  server: { fs: { allow: [resolve(__dirname, "../..") ] } },
  build: {
    rollupOptions: {
      input: {
        index: resolve(__dirname, "index.html"),
        worker: resolve(__dirname, "../../packages/browser-runtime/src/shared-worker.ts"),
      },
      output: {
        entryFileNames: (chunk) => chunk.name === "worker" ? "worker.js" : "assets/[name]-[hash].js",
        chunkFileNames: "assets/[name]-[hash].js",
        assetFileNames: "assets/[name]-[hash][extname]",
      },
    },
  },
});
