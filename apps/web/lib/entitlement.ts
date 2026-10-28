// Who gets Pro. Dodo keeps a subscription `active` until the paid period ends even when the
// customer cancels at period end, so `cancelled` means access is over.

export type SubscriptionState = {
  status: string;
  currentPeriodEnd: Date | null;
  pastDueEndsAt: Date | null;
};

// A renewal webhook can arrive late; don't cut off a paying user the minute the period rolls.
const RENEWAL_GRACE_MS = 3 * 24 * 60 * 60 * 1000;

export function isEntitled(subs: readonly SubscriptionState[], now: Date): boolean {
  return subs.some((s) => {
    if (s.status === "active") {
      return !s.currentPeriodEnd || s.currentPeriodEnd.getTime() + RENEWAL_GRACE_MS > now.getTime();
    }
    if (s.status === "past_due") {
      const end = s.pastDueEndsAt ?? (s.currentPeriodEnd && new Date(s.currentPeriodEnd.getTime() + RENEWAL_GRACE_MS));
      return !!end && end > now;
    }
    return false;
  });
}
