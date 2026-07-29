import { defineConfig } from "vite-plus";
import solid from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";
import { playwright } from "@vitest/browser-playwright";
import { SHIKI_LANGS } from "./src/lib/langs";

export default defineConfig({
  plugins: [solid(), tailwindcss()],
  server: {
    port: 7823,
    strictPort: true,
  },
  build: {
    target: "esnext",
    outDir: "dist",
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            {
              // 上游把 Langium 打进单一 parser module，命名后由 chunk budget 对它单独设限。
              name: "mermaid-parser-runtime",
              test: /node_modules[\\/]@mermaid-js[\\/]parser[\\/]/,
            },
          ],
        },
      },
    },
  },
  // shiki 细粒度子路径依赖：不预声明会在 dev/test 首跑时触发 dep optimizer 二次扫描，
  // browser mode 下页面中途 reload 直接 flaky（vitest 报 "unexpectedly reloaded a test"）。
  // 语言子路径由 SHIKI_LANGS 派生（与 markdown.ts 高亮白名单同一份清单，勿再手工列第二遍）
  // 大依赖全部预打包：未预打包的模块经按需服务逐个下发，测试文件收尾时 in-flight 请求
  // 撞 page 导航被 playwright GC（route.fulfill: object collected）——预打包把请求数压到个位数
  optimizeDeps: {
    include: [
      "solid-js",
      "solid-js/web",
      "solid-js/store",
      "class-variance-authority",
      "marked",
      "dompurify",
      "mermaid",
      "@tauri-apps/api",
      "@tauri-apps/api/core",
      "@tauri-apps/api/event",
      "@tauri-apps/api/window",
      "@tauri-apps/plugin-dialog",
      "@tauri-apps/plugin-process",
      "@tauri-apps/plugin-updater",
      "@tauri-apps/plugin-websocket",
      "shiki/core",
      "shiki/engine/javascript",
      "shiki/themes/github-dark.mjs",
      "shiki/themes/github-light.mjs",
      ...SHIKI_LANGS.map((l) => `shiki/langs/${l}.mjs`),
    ],
  },
  test: {
    // 显式 node：vite-plugin-solid 在 mode=test 且未设 environment 时注入 jsdom，
    // vitest 4 启动时对该 environment 做依赖检查，jsdom 未装则 exit 1（browser 测试实际不用它）
    environment: "node",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
    setupFiles: ["src/test-setup.ts"],
    coverage: {
      provider: "istanbul",
      include: ["src/**/*.{ts,tsx}"],
      exclude: ["src/**/*.test.{ts,tsx}", "src/test-setup.ts"],
      reporter: ["json"],
    },
    browser: {
      enabled: true,
      provider: playwright(),
      headless: true,
      instances: [{ browser: "webkit", viewport: { width: 1280, height: 800 } }],
    },
  },
});
