// The Displays page's one plain sentence (design/web-console-overhaul.md D5, §5.2).
//
// The page's problem was never that the policy was hard to set — it was that six axes, a badge
// row and 2 kB of help text never answered the only question an operator actually has: what
// happens to my screens when a device connects. This says it in one sentence, and the same
// function captions every preset card, so picking a preset means reading what it will do.
//
// Clauses are whole messages, not glued fragments: German puts the verb somewhere English does
// not, and a sentence assembled from four half-phrases cannot be translated.
import type { EffectivePolicy, KeepAlive } from "@/api/gen/model";
import { m } from "@/paraglide/messages";

export interface DescribeOptions {
	/**
	 * A session is streaming right now. The host applies a policy at the next connect, so this
	 * is the only condition under which the "not yet" tail is true — stating it unconditionally
	 * is what made the old pending note noise on an idle host.
	 */
	live?: boolean;
	/** Name the device instead of "each device" — the per-device sheet (§6.2). */
	deviceName?: string;
}

/** `{who} gets {screen}, {kept}. {monitors}. [Applies to the next connection.]` */
export function describePolicy(
	policy: EffectivePolicy,
	opts: DescribeOptions = {},
): string {
	const parts = [
		firstSentence(policy, opts),
		secondDevice(policy.mode_conflict),
	];
	if (opts.live) parts.push(m.display_says_next_connect());
	return parts.join(" ");
}

function firstSentence(policy: EffectivePolicy, opts: DescribeOptions): string {
	const kept = keptClause(policy.keep_alive);
	// A named device is one device, so it never reads as "each"; an idle host has no device yet,
	// so it speaks about the next one.
	if (opts.deviceName) {
		return m.display_says_device({
			name: opts.deviceName,
			kept,
			...screen(policy),
		});
	}
	return m.display_says_each({ kept, ...screen(policy) });
}

/** The screen a device is given, as a fragment the sentence messages interpolate. */
function screen(policy: EffectivePolicy): { screen: string } {
	switch (policy.topology) {
		case "extend":
			return { screen: m.display_says_screen_extend() };
		case "primary":
			return { screen: m.display_says_screen_primary() };
		case "exclusive":
			return { screen: m.display_says_screen_exclusive() };
		default:
			// `auto` is the host choosing per setup — a headless box has nothing to extend from.
			// Naming a specific outcome here would be a guess the host has not made yet.
			return { screen: m.display_says_screen_auto() };
	}
}

function keptClause(keep: KeepAlive): string {
	switch (keep.mode) {
		case "off":
			return m.display_says_kept_off();
		case "forever":
			return m.display_says_kept_forever();
		default:
			return m.display_says_kept_for({ seconds: keep.seconds });
	}
}

function secondDevice(conflict: EffectivePolicy["mode_conflict"]): string {
	switch (conflict) {
		case "steal":
			return m.display_says_second_steal();
		case "join":
			return m.display_says_second_join();
		case "reject":
			return m.display_says_second_reject();
		default:
			return m.display_says_second_separate();
	}
}
