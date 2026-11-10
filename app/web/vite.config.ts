import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";
import { APP_NAME } from "../shared/identity.mjs";

const port = Number(process.env.LEARNER_PORT ?? 4317);

export default defineConfig({
  root: fileURLToPath(new URL(".", import.meta.url)),
  plugins: [
    react(),
    tailwindcss(),
    // the desktop window title is the document title, so fill it at build time
    {
      name: "app-name",
      transformIndexHtml: {
        order: "pre",
        handler: (html: string) => html.replaceAll("%APP_NAME%", APP_NAME),
      },
    },
  ],
  build: { outDir: "dist", emptyOutDir: true },
  server: {
    port: 5173,
    proxy: {
      "/api": `http://127.0.0.1:${port}`,
      "/files": `http://127.0.0.1:${port}`,
    },
  },
});
