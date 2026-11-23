"use client";
import { Cloud, CloudAlert, Snowflake, type LucideIcon } from 'lucide-react';
import { useI18n } from '@/lib/i18n';
import type { Cartridge, CopyBucketState } from '@/lib/api';
import type { MediaObservation } from '@/lib/telemetry';

type Badge = { tone: 'available' | 'cold' | 'unknown'; icon: LucideIcon; label: string; busy?: boolean };

// What the bucket holding the tape's latest verified copy allows, as OVH last reported it.
const badges: Record<CopyBucketState, Badge> = {
  available: { tone: 'available', icon: Cloud, label: 'Copie sur S3' },
  archiving: { tone: 'cold', icon: Snowflake, label: 'Copie sur S3 · archivage cold en cours', busy: true },
  archived: { tone: 'cold', icon: Snowflake, label: 'Copie sur S3 · archivée cold' },
  recalling: { tone: 'cold', icon: Snowflake, label: 'Copie sur S3 · archivée cold, sortie d’archive en cours', busy: true },
  recalled: { tone: 'available', icon: Cloud, label: 'Copie sur S3 · sortie d’archive, lecture seule' },
  unknown: { tone: 'unknown', icon: CloudAlert, label: 'Copie sur S3 · état du bucket inconnu' },
};

/** Sticker on the tape itself: where its latest verified copy lives. Nothing without a copy. */
export function MediaCopyBadge({ media, observation }: { media: Cartridge; observation?: MediaObservation }) {
  const { t, locale } = useI18n();
  const copy = media.last_copy;
  if (!copy) return null;
  const badge = badges[copy.bucket_state ?? 'available'] ?? badges.unknown;
  // Veeam or PBS wrote to the tape after its copy: the local volume outgrew what was copied.
  const local = observation?.local_size_bytes;
  const stale = copy.bytes != null && local != null && local > copy.bytes;
  const Icon = badge.icon;
  const details = [
    t(badge.label),
    stale ? t('antérieure aux dernières écritures') : null,
    copy.bucket,
    t('génération {generation}', { generation: copy.generation }),
    t('vérifiée le {date}', { date: new Date(copy.checked_at).toLocaleDateString(locale) }),
  ].filter(Boolean).join(' · ');
  return (
    <span className="lto-copy-badge" role="img" data-tone={badge.tone} data-stale={stale} data-busy={!!badge.busy} aria-label={details} title={details}>
      <Icon aria-hidden="true" />
    </span>
  );
}
