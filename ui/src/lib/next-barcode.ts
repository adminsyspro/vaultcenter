/** Continue the most common numbered series in the selected library. */
export function nextBarcode(barcodes: string[], mediaType: string): string {
  const suffix = /^LTO([0-9])$/.exec(mediaType)?.[1];
  const targetSuffix = suffix ? `L${suffix}` : '';
  const series = new Map<string, {prefix: string; suffix: string; count: number; maximum: bigint; width: number}>();
  for (const barcode of barcodes) {
    const ending = barcode.match(/L[0-9A-Z]$/)?.[0] ?? '';
    const serial = ending ? barcode.slice(0, -2) : barcode;
    const match = /^(.*?)(\d+)$/.exec(serial);
    if (!match) continue;
    const [, prefix, digits] = match;
    const key = `${prefix}|${ending}`;
    const value = BigInt(digits);
    const group = series.get(key) ?? {prefix, suffix: ending, count: 0, maximum: BigInt(0), width: 0};
    group.count++;
    if (value > group.maximum) group.maximum = value;
    group.width = Math.max(group.width, digits.length);
    series.set(key, group);
  }
  const groups = [...series.values()].sort((a, b) =>
    Number(b.suffix === targetSuffix) - Number(a.suffix === targetSuffix) ||
    b.count - a.count || a.prefix.localeCompare(b.prefix) || a.suffix.localeCompare(b.suffix));
  const selected = groups[0];
  const prefix = selected?.prefix ?? 'CVT';
  const ending = targetSuffix || selected?.suffix || '';
  const width = selected?.width ?? 3;
  let sequence = (selected?.maximum ?? BigInt(0)) + BigInt(1);
  const existing = new Set(barcodes);
  for (;;) {
    const candidate = `${prefix}${sequence.toString().padStart(width, '0')}${ending}`;
    // Leave manual entry available when a series has exhausted barcode length.
    if (candidate.length > 16) return '';
    if (candidate.length >= 6 && !existing.has(candidate)) return candidate;
    if (candidate.length < 6) return '';
    sequence++;
  }
}
