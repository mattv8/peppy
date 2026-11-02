import { resolve } from "node:path";
import { realpathSync } from "node:fs";
import { defineConfig, normalizePath } from "vite";
import react from "@vitejs/plugin-react";

const desktopBridgePath = normalizePath(realpathSync(resolve(__dirname, "../desktop/src/bridge/bridge.ts")));
const webBridgePath = normalizePath(resolve(__dirname, "src/ui-bridge.ts"));

export default defineConfig({
  plugins: [
    react(),
    {
      name: "replace-desktop-bridge",
      enforce: "pre",
      async resolveId(source, importer, options) {
        const resolved = await this.resolve(source, importer, { ...options, skipSelf: true });
        const resolvedPath = resolved && normalizePath(resolved.id.split("?")[0]);
        return resolvedPath === desktopBridgePath ? webBridgePath : null;
      },
    },
  ],
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
