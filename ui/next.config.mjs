/** @type {import('next').NextConfig} */
const config = {
  output: "export",
  distDir: process.env.VAULTCENTER_DEV_DEMO === "1" ? ".next-demo" : ".next",
  outputFileTracingRoot: import.meta.dirname,
  trailingSlash: true,
  poweredByHeader: false,
  images: { unoptimized: true },
  // Use the compiler API provided by our TypeScript 5 dependency.
  experimental: { useTypeScriptCli: false },
};

export default config;
