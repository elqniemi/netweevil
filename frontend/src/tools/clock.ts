// Trip times are seconds on the caller's clock (seconds since midnight in the
// console). The form shows them as HH:MM and accepts plain seconds too.

/** Seconds from "HH:MM", "HH:MM:SS" or a plain number of seconds; undefined when blank. */
export function parseClock(value: unknown): number | undefined {
  if (value === undefined || value === null) return undefined;
  if (typeof value === "number") return Number.isFinite(value) ? value : undefined;
  const text = String(value).trim();
  if (text === "") return undefined;
  const clock = text.match(/^(\d{1,3}):(\d{2})(?::(\d{2}))?$/);
  if (clock) {
    const [, h, m, s] = clock;
    if (Number(m) > 59 || Number(s ?? 0) > 59) throw new Error(`'${text}' is not a valid time`);
    return Number(h) * 3600 + Number(m) * 60 + Number(s ?? 0);
  }
  const seconds = Number(text);
  if (Number.isFinite(seconds) && seconds >= 0) return seconds;
  throw new Error(`'${text}' is not a time (use HH:MM or seconds)`);
}

/** "HH:MM" (or "HH:MM:SS" when seconds are not whole minutes); hours run past 24. */
export function formatClock(seconds: number | undefined | null): string {
  if (seconds === undefined || seconds === null || !Number.isFinite(seconds)) return "";
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const rest = s % 60;
  const hm = `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
  return rest ? `${hm}:${String(rest).padStart(2, "0")}` : hm;
}

/** Spreadsheet-style stop letters: A..Z, AA, AB, ... */
export function stopLetter(index: number): string {
  let n = index;
  let out = "";
  do {
    out = String.fromCharCode(65 + (n % 26)) + out;
    n = Math.floor(n / 26) - 1;
  } while (n >= 0);
  return out;
}
