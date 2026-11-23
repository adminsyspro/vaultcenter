import type { Metadata } from "next";
import localFont from "next/font/local";
import { GeistMono } from "geist/font/mono";
import { Providers } from "./_components/providers";
import "./globals.css";

const manrope = localFont({ src: "./fonts/Manrope.ttf", variable: "--font-manrope", weight: "200 800", display: "swap" });

export const metadata: Metadata = {
  title: "VaultCenter · Console",
  description: "Gestion des cartouches virtuelles et des exports S3.",
  icons: { icon: [{ url: "/brand/vaultcenter-icon-light.svg", type: "image/svg+xml", media: "(prefers-color-scheme: light)" }, { url: "/brand/vaultcenter-icon-dark.svg", type: "image/svg+xml", media: "(prefers-color-scheme: dark)" }], apple: "/brand/vaultcenter.png" },
  robots: { index: false, follow: false },
};

export default function Layout({ children }: { children: React.ReactNode }) {
  return (
    <html
      lang="fr"
      className={`${manrope.variable} ${GeistMono.variable}`}
      suppressHydrationWarning
    >
      <body className="antialiased">
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
