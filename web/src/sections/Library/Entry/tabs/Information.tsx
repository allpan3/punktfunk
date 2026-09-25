import type { FC } from "react";
import { m } from "@/paraglide/messages";
import { Group, TextField } from "../fields";
import type { FormState } from "../model";
import type { TabProps } from "./types";

const META: (keyof FormState)[] = [
	"description",
	"developer",
	"publisher",
	"releaseYear",
	"players",
	"platform",
	"region",
	"genres",
	"tags",
];

/** Title and the `GameMeta` fields. */
export const InformationTab: FC<TabProps> = ({ draft, set, readOnly }) => {
	const field = (key: keyof FormState) => ({
		id: key,
		value: draft[key] as string,
		onChange: (v: string) => set(key, v),
		readOnly,
	});
	if (readOnly && META.every((k) => draft[k] === "")) {
		return (
			<Group title={m.library_entry_tab_information()}>
				<p className="text-sm text-muted-foreground">
					{m.library_entry_no_details()}
				</p>
			</Group>
		);
	}
	return (
		<Group title={m.library_entry_tab_information()}>
			{!readOnly && (
				<TextField
					{...field("title")}
					label={m.library_field_title()}
					required
				/>
			)}
			<TextField
				{...field("description")}
				label={m.library_field_description()}
				multiline
			/>
			<div className="grid gap-4 @lg:grid-cols-2">
				<TextField
					{...field("developer")}
					label={m.library_field_developer()}
				/>
				<TextField
					{...field("publisher")}
					label={m.library_field_publisher()}
				/>
				{/* `type="number"` over a string: both are optional, and empty means unset. */}
				<TextField
					{...field("releaseYear")}
					label={m.library_field_release_year()}
					type="number"
				/>
				<TextField
					{...field("players")}
					label={m.library_field_players()}
					type="number"
				/>
				<TextField
					{...field("platform")}
					label={m.library_field_platform()}
					help={m.library_field_platform_help()}
				/>
				<TextField
					{...field("region")}
					label={m.library_field_region()}
					help={m.library_field_region_help()}
				/>
				<TextField
					{...field("genres")}
					label={m.library_field_genres()}
					help={m.library_field_genres_help()}
				/>
				<TextField
					{...field("tags")}
					label={m.library_field_tags()}
					help={m.library_field_tags_help()}
				/>
			</div>
		</Group>
	);
};
