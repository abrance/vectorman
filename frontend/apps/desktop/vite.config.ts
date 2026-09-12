import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [react()],
  server: {
    host: "0.0.0.0",
    port: 5174,
    allowedHosts: [".monkeycode-ai.online"],
    proxy: {
      "/api": { target: "http://127.0.0.1:7200", changeOrigin: true },
    },
  },
  preview: {
    host: "0.0.0.0",
    port: 5174,
    allowedHosts: [".monkeycode-ai.online"],
    proxy: {
      "/api": { target: "http://127.0.0.1:7200", changeOrigin: true },
    },
  },
});
