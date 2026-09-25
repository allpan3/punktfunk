import type { GameEntry } from "@/api/gen/model/gameEntry";
import { m } from "@/paraglide/messages";

/** The custom-CRUD path param is the raw id without the `custom:` prefix. */
export function customId(entry: GameEntry): string {
	return entry.id.startsWith("custom:")
		? entry.id.slice("custom:".length)
		: entry.id;
}

/** The operator owns this entry and may edit or delete it. A provider-synced custom entry is
 * refused by the host (409), so it counts as managed. */
export function isOperatorOwned(entry: GameEntry): boolean {
	return entry.store === "custom" && !entry.provider;
}

/**
 * Display label for a store badge. Steam and custom keep their localized strings; any other store
 * shows its source's name (`nameOf`), or its id capitalized where nothing names it.
 */
export function storeLabel(
	store: string,
	nameOf?: (id: string) => string | undefined,
): string {
	switch (store) {
		case "custom":
			return m.library_store_custom();
		case "steam":
			return m.library_store_steam();
		default:
			return nameOf?.(store) ?? store.charAt(0).toUpperCase() + store.slice(1);
	}
}
