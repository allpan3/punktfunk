import { ImageOff } from "lucide-react";
import { type FC, useState } from "react";
import { LAUNCHER_ICONS, LauncherIcon } from "@/components/launcher-icon";
import { Label } from "@/components/ui/label";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import { m } from "@/paraglide/messages";
import { Group, ReadRow, TextField } from "../fields";
import type { FormState } from "../model";
import type { TabProps } from "./types";

type ArtKind = "portrait" | "hero" | "header" | "logo";

const SLOTS: {
	kind: ArtKind;
	label: () => string;
	help: () => string;
	frame: string;
}[] = [
	{
		kind: "portrait",
		label: m.library_media_portrait,
		help: m.library_media_portrait_help,
		frame: "aspect-[2/3] max-w-48",
	},
	{
		kind: "hero",
		label: m.library_media_hero,
		help: m.library_media_hero_help,
		frame: "aspect-[96/31]",
	},
	{
		kind: "header",
		label: m.library_media_header,
		help: m.library_media_header_help,
		frame: "aspect-[92/43] max-w-80",
	},
	{
		kind: "logo",
		label: m.library_media_logo,
		help: m.library_media_logo_help,
		frame: "aspect-[16/9] max-w-80",
	},
];

const NO_ICON = "none";

/** What the preview shows: a URL straight from the browser, a stored local path through the
 * host's art proxy, and nothing for a local path typed since the last save. */
export function previewSrc(
	kind: ArtKind,
	draft: FormState,
	baseline: FormState,
	proxied: string | null | undefined,
	readOnly: boolean,
): string | null | undefined {
	if (readOnly) return proxied ?? null;
	const v = draft[kind].trim();
	if (!v) return null;
	if (/^(https?:|data:)/.test(v)) return v;
	return v === baseline[kind].trim() ? (proxied ?? null) : undefined;
}

const Preview: FC<{
	src: string | null | undefined;
	frame: string;
	contain: boolean;
}> = ({ src, frame, contain }) => {
	const [failed, setFailed] = useState<string | null>(null);
	const broken = src != null && failed === src;
	return (
		<div
			className={`${frame} flex w-full items-center justify-center overflow-hidden rounded-md border bg-muted text-muted-foreground`}
		>
			{src && !broken ? (
				<img
					src={src}
					alt=""
					className={`size-full ${contain ? "object-contain p-2" : "object-cover"}`}
					onError={() => setFailed(src)}
				/>
			) : (
				<div className="flex flex-col items-center gap-1 p-2 text-center text-xs">
					<ImageOff className="size-4" />
					{broken
						? m.library_entry_art_failed()
						: src === undefined
							? m.library_entry_art_after_save()
							: m.library_entry_art_none()}
				</div>
			)}
		</div>
	);
};

/** The four artwork slots with previews, and the brand mark. */
export const MediaTab: FC<TabProps> = ({
	draft,
	set,
	readOnly,
	entry,
	baseline,
}) => (
	<>
		<Group title={m.library_entry_tab_media()}>
			<div className="grid gap-6 @2xl:grid-cols-2">
				{SLOTS.map(({ kind, label, help, frame }) => (
					<div key={kind} className="space-y-3">
						<Preview
							src={previewSrc(
								kind,
								draft,
								baseline,
								entry?.art[kind],
								readOnly,
							)}
							frame={frame}
							contain={kind === "logo"}
						/>
						{readOnly ? (
							<p className="text-xs font-medium text-muted-foreground">
								{label()}
							</p>
						) : (
							<TextField
								id={kind}
								label={label()}
								value={draft[kind]}
								onChange={(v) => set(kind, v)}
								help={help()}
								type="url"
							/>
						)}
					</div>
				))}
			</div>
		</Group>
		<Group title={m.library_entry_icon()} help={m.library_entry_icon_help()}>
			<div className="flex items-center gap-4">
				<div className="flex size-14 shrink-0 items-center justify-center rounded-md border bg-muted text-muted-foreground [&>svg]:size-8">
					<LauncherIcon icon={draft.icon || null} />
				</div>
				{readOnly ? (
					<ReadRow label={m.library_entry_icon()} value={draft.icon} />
				) : (
					<div className="w-full max-w-60 space-y-2">
						<Label htmlFor="entry-icon" className="sr-only">
							{m.library_entry_icon()}
						</Label>
						<Select
							value={draft.icon || NO_ICON}
							onValueChange={(v) => set("icon", v === NO_ICON ? "" : v)}
						>
							<SelectTrigger id="entry-icon" size="sm">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								<SelectItem value={NO_ICON}>
									{m.library_entry_icon_none()}
								</SelectItem>
								{Object.keys(LAUNCHER_ICONS).map((token) => (
									<SelectItem key={token} value={token}>
										{token.charAt(0).toUpperCase() + token.slice(1)}
									</SelectItem>
								))}
							</SelectContent>
						</Select>
					</div>
				)}
			</div>
		</Group>
	</>
);
