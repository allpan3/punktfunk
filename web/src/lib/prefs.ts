// Per-browser console preferences (design/web-console-overhaul.md D8). Nothing here reaches
// the host: these are view choices, and a phone and a desk are allowed to disagree.
//
// `useSyncExternalStore` rather than useState + useEffect because the console server-renders.
// The server snapshot is the fallback and the client snapshot is the stored value, so React
// swaps them itself instead of us hydrating with the wrong one and correcting it in an effect.
import { useCallback, useSyncExternalStore } from "react";

const listeners = new Set<() => void>();

const subscribe = (fn: () => void) => {
	listeners.add(fn);
	return () => {
		listeners.delete(fn);
	};
};

// `getSnapshot` must be referentially stable or React re-renders forever, and `JSON.parse`
// hands back a fresh object every call. Keyed on the raw string, so an external write (another
// tab, devtools) still invalidates.
const parsed = new Map<string, { raw: string | null; value: unknown }>();

function read<T>(key: string, fallback: T, valid: (v: unknown) => v is T): T {
	let raw: string | null = null;
	try {
		raw = window.localStorage.getItem(key);
	} catch {
		// Private mode, or storage disabled: the choice still holds for this session, it just
		// does not survive a reload.
		const session = parsed.get(key);
		return session ? (session.value as T) : fallback;
	}
	const hit = parsed.get(key);
	if (hit && hit.raw === raw) return hit.value as T;
	let value = fallback;
	if (raw !== null) {
		try {
			const candidate: unknown = JSON.parse(raw);
			if (valid(candidate)) value = candidate;
		} catch {
			// A hand-edited or stale-shape value reads as absent rather than breaking the page.
		}
	}
	parsed.set(key, { raw, value });
	return value;
}

/**
 * A `useState`-shaped preference backed by `localStorage`, validated on every read.
 *
 * `valid` is what keeps a value written by an older console (or a plugin id since removed)
 * from reaching a component that cannot render it.
 */
export function useLocalPref<T>(
	key: string,
	fallback: T,
	valid: (v: unknown) => v is T,
): [T, (next: T) => void] {
	const value = useSyncExternalStore(
		subscribe,
		() => read(key, fallback, valid),
		() => fallback,
	);
	const set = useCallback(
		(next: T) => {
			const raw = JSON.stringify(next);
			try {
				window.localStorage.setItem(key, raw);
			} catch {
				// Unwritable storage still gets the change for this session, from the cache below.
			}
			parsed.set(key, { raw, value: next });
			for (const fn of listeners) fn();
		},
		[key],
	);
	return [value, set];
}

export const isBoolean = (v: unknown): v is boolean => typeof v === "boolean";
