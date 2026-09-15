import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

const backend = { target: "http://127.0.0.1:8081", changeOrigin: true };

export default defineConfig({
  plugins: [react()],
  resolve: {
    dedupe: ["react", "react-dom"],
  },
  server: {
    host: "0.0.0.0",
    port: 5175,
    allowedHosts: [".monkeycode-ai.online"],
    proxy: {
      "/v1": backend,
      "/api": backend,
      "/health": backend,
    },
  },
  preview: {
    host: "0.0.0.0",
    port: 5175,
    allowedHosts: [".monkeycode-ai.online"],
    proxy: {
      "/v1": backend,
      "/api": backend,
      "/health": backend,
    },
  },
});
