import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  resolve: {
    dedupe: ["react", "react-dom"],
  },
  server: {
    host: "0.0.0.0",
    port: 5173,
    allowedHosts: [".monkeycode-ai.online"],
    proxy: {
      "/api/gse": {
        target: "http://127.0.0.1:7101",
        changeOrigin: true,
      },
      "/api/sql": {
        target: "http://127.0.0.1:8081",
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api\/sql/, ""),
      },
      "/api/prom": {
        target: "http://127.0.0.1:9090",
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api\/prom/, ""),
      },
    },
  },
  preview: {
    host: "0.0.0.0",
    port: 5173,
    allowedHosts: [".monkeycode-ai.online"],
    proxy: {
      "/api/gse": {
        target: "http://127.0.0.1:7101",
        changeOrigin: true,
      },
      "/api/sql": {
        target: "http://127.0.0.1:8081",
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api\/sql/, ""),
      },
      "/api/prom": {
        target: "http://127.0.0.1:9090",
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/api\/prom/, ""),
      },
    },
  },
});
