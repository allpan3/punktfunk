import { useQueryClient } from "@tanstack/react-query";
import { Pencil, Plus, X } from "lucide-react";
import { type FC, type FormEvent, useState } from "react";
import {
	getGetLibraryQueryKey,
	useCreateCustomGame,
	useGetCustomGame,
	useUpdateCustomGame,
} from "@/api/gen/library/library";
import type { AudioSessions } from "@/api/gen/model/audioSessions";
import type { CustomInput } from "@/api/gen/model/customInput";
import type { GameEntry } from "@/api/gen/model/gameEntry";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
	Select,
	SelectContent,
	SelectItem,
	SelectTrigger,
	SelectValue,
} from "@/components/ui/select";
import { apiErrorMessage } from "@/lib/errors";
import { m } from "@/paraglide/messages";
import {
	emptyForm,
	type FormState,
	formFrom,
	needsPassword,
	toInput,
	withPassword,
	withStored,
} from "./Entry/model";
import { customId } from "./helpers";

/** What the form targets: an existing custom entry to edit, or "new" for a fresh add. */
export type FormTarget = GameEntry | "new";

/**
 * Container: the add/edit form — owns the create + update mutations and derives the
 * initial field state from the target. Kept entirely separate from the overview grid
 * (own file, own queries) so the two concerns don't share a component.
 */
export const GameFormSection: FC<{
	target: FormTarget;
	onClose: () => void;
}> = ({ target, onClose }) => {
	const qc = useQueryClient();
	const create = useCreateCustomGame();
	const update = useUpdateCustomGame();
	const invalidate = () =>
		qc.invalidateQueries({ queryKey: getGetLibraryQueryKey() });

	// A rejected save must not close the form and must not look like a success. It used to do both:
	// nothing read `create.error`/`update.error`, and the un-caught `mutateAsync` rejection meant
	// the entry silently didn't save while the dialog disappeared — taking the operator's typing
	// with it.
	const onSubmit = async (data: CustomInput) => {
		try {
			if (target === "new") await create.mutateAsync({ data });
			else await update.mutateAsync({ id: customId(target), data });
		} catch {
			return; // the message is rendered from the mutation's own error state below
		}
		invalidate();
		onClose();
	};

	// Edit waits for the host's own copy of the row: the catalog entry carries neither `detect`
	// nor `prep`, and a form seeded without them would have nothing to round-trip.
	const stored = useGetCustomGame(target === "new" ? "" : customId(target), {
		query: { enabled: target !== "new" },
	});
	if (target !== "new" && stored.isPending) return null;
	return (
		<GameForm
			initial={
				target === "new" ? emptyForm : withStored(formFrom(target), stored.data)
			}
			mode={target === "new" ? "add" : "edit"}
			onSubmit={onSubmit}
			onCancel={onClose}
			isSaving={create.isPending || update.isPending}
			error={apiErrorMessage(create.error ?? update.error)}
		/>
	);
};

/** One labeled text input bound to a FormState key — the form is a stack of these. */
const Field: FC<{
	id: string;
	label: string;
	value: string;
	onChange: (value: string) => void;
	help?: string;
	type?: string;
	required?: boolean;
}> = ({ id, label, value, onChange, help, type, required }) => (
	<div className="space-y-2">
		<Label htmlFor={`lib-${id}`}>{label}</Label>
		<Input
			id={`lib-${id}`}
			type={type}
			inputMode={
				type === "url" ? "url" : type === "number" ? "numeric" : undefined
			}
			required={required}
			value={value}
			onChange={(e) => onChange(e.target.value)}
		/>
		{help && <p className="text-xs text-muted-foreground">{help}</p>}
	</div>
);

/**
 * The add/edit form card. Owns only its own field state (re-seeded per mount — the
 * parent keys it by target); reports a ready-to-send `CustomInput` on submit.
 */
export const GameForm: FC<{
	initial: FormState;
	mode: "add" | "edit";
	onSubmit: (data: CustomInput) => void;
	onCancel: () => void;
	isSaving: boolean;
	/** The host's refusal, if the last save failed — shown next to the button that caused it. */
	error?: string;
}> = ({ initial, mode, onSubmit, onCancel, isSaving, error }) => {
	const [form, setForm] = useState<FormState>(initial);
	// The console password; never part of the entry, so never in the draft.
	const [password, setPassword] = useState("");
	const set = (key: keyof FormState) => (value: string) =>
		setForm((f) => ({ ...f, [key]: value }));
	const gated = needsPassword(toInput(form));

	const handleSubmit = (e: FormEvent) => {
		e.preventDefault();
		const data = toInput(form);
		if (!data.title) return;
		if (gated && !password) return;
		onSubmit(withPassword(data, password));
	};

	return (
		<Card className="max-w-xl">
			<CardHeader className="flex-row items-center justify-between space-y-0">
				<CardTitle className="flex items-center gap-2">
					{mode === "edit" ? (
						<Pencil className="size-4" />
					) : (
						<Plus className="size-4" />
					)}
					{mode === "edit" ? m.library_edit_title() : m.library_add_title()}
				</CardTitle>
				<Button
					variant="ghost"
					size="icon"
					aria-label={m.library_cancel()}
					onClick={onCancel}
				>
					<X className="size-4" />
				</Button>
			</CardHeader>
			<CardContent>
				<form onSubmit={handleSubmit} className="space-y-4">
					<Field
						id="title"
						label={m.library_field_title()}
						value={form.title}
						onChange={set("title")}
						required
					/>
					<Field
						id="portrait"
						label={m.library_field_portrait()}
						value={form.portrait}
						onChange={set("portrait")}
						type="url"
					/>
					<Field
						id="hero"
						label={m.library_field_hero()}
						value={form.hero}
						onChange={set("hero")}
						type="url"
					/>
					<Field
						id="header"
						label={m.library_field_header()}
						value={form.header}
						onChange={set("header")}
						type="url"
					/>
					<Field
						id="logo"
						label={m.library_field_logo()}
						value={form.logo}
						onChange={set("logo")}
						type="url"
					/>
					<Field
						id="command"
						label={m.library_field_command()}
						value={form.command}
						onChange={set("command")}
						help={m.library_field_command_help()}
					/>
					{/* Saving a command, or a row that carries prep, runs code as the host user: the
						    console password is asked for exactly when the BFF gate applies. */}
					{gated && (
						<Field
							id="password"
							label={m.library_field_password()}
							value={password}
							onChange={setPassword}
							help={m.library_field_password_help()}
							type="password"
							required
						/>
					)}
					{/* Design D4: a launcher entry opens the launcher itself rather than a title. It
					    launches and leases like any other entry — this only moves it into the
					    console's Launchers rail. Hand-adding one is the supported way to get a
					    "Heroic" or "Lutris" tile without installing that source's plugin. */}
					<div className="space-y-2">
						<div className="flex items-center gap-2">
							<Checkbox
								id="lib-isLauncher"
								checked={form.isLauncher}
								onCheckedChange={(next) =>
									setForm((f) => ({ ...f, isLauncher: next === true }))
								}
							/>
							<Label htmlFor="lib-isLauncher">{m.library_field_role()}</Label>
						</div>
						<p className="text-xs text-muted-foreground">
							{m.library_field_role_help()}
						</p>
					</div>
					<fieldset className="space-y-4 border-t pt-2">
						<legend className="sr-only">{m.library_process_legend()}</legend>
						<p
							aria-hidden
							className="text-sm font-medium text-muted-foreground"
						>
							{m.library_process_legend()}
						</p>
						<p className="text-xs text-muted-foreground">
							{m.library_process_help()}
						</p>
						<Field
							id="exe"
							label={m.library_field_exe()}
							value={form.exe}
							onChange={set("exe")}
							help={m.library_field_exe_help()}
						/>
						<Field
							id="installDir"
							label={m.library_field_install_dir()}
							value={form.installDir}
							onChange={set("installDir")}
							help={m.library_field_install_dir_help()}
						/>
						<Field
							id="processName"
							label={m.library_field_process_name()}
							value={form.processName}
							onChange={set("processName")}
							help={m.library_field_process_name_help()}
						/>
					</fieldset>
					<div className="space-y-1 border-t pt-4">
						<Label htmlFor="lib-audio">{m.library_field_audio()}</Label>
						<Select
							value={form.audioSessions}
							onValueChange={(v) =>
								setForm((f) => ({ ...f, audioSessions: v as AudioSessions }))
							}
						>
							<SelectTrigger id="lib-audio" size="sm">
								<SelectValue />
							</SelectTrigger>
							<SelectContent>
								<SelectItem value="all">{m.library_audio_all()}</SelectItem>
								<SelectItem value="owner">{m.library_audio_owner()}</SelectItem>
								<SelectItem value="joined">
									{m.library_audio_joined()}
								</SelectItem>
								<SelectItem value="launcher">
									{m.library_audio_launcher()}
								</SelectItem>
							</SelectContent>
						</Select>
						<p className="text-xs text-muted-foreground">
							{m.library_field_audio_help()}
						</p>
					</div>
					<fieldset className="space-y-4 border-t pt-2">
						<legend className="sr-only">{m.library_details_legend()}</legend>
						<p
							aria-hidden
							className="text-sm font-medium text-muted-foreground"
						>
							{m.library_details_legend()}
						</p>
						<Field
							id="platform"
							label={m.library_field_platform()}
							value={form.platform}
							onChange={set("platform")}
							help={m.library_field_platform_help()}
						/>
						<Field
							id="description"
							label={m.library_field_description()}
							value={form.description}
							onChange={set("description")}
						/>
						<div className="grid grid-cols-2 gap-4">
							<Field
								id="developer"
								label={m.library_field_developer()}
								value={form.developer}
								onChange={set("developer")}
							/>
							<Field
								id="publisher"
								label={m.library_field_publisher()}
								value={form.publisher}
								onChange={set("publisher")}
							/>
						</div>
						{/* These two stay `type="number"` over a STRING field rather than becoming
						    `InputNumber` like the policy card's numbers: both are OPTIONAL metadata
						    where empty means "don't send it" (see `int()` above), and InputNumber's
						    contract is `value: number` — it cannot express "unset", so adopting it
						    would invent a year for every entry that hasn't got one. The type here
						    only asks for a numeric keypad. */}
						<div className="grid grid-cols-2 gap-4">
							<Field
								id="releaseYear"
								label={m.library_field_release_year()}
								value={form.releaseYear}
								onChange={set("releaseYear")}
								type="number"
							/>
							<Field
								id="players"
								label={m.library_field_players()}
								value={form.players}
								onChange={set("players")}
								type="number"
							/>
						</div>
						<Field
							id="region"
							label={m.library_field_region()}
							value={form.region}
							onChange={set("region")}
							help={m.library_field_region_help()}
						/>
						<Field
							id="genres"
							label={m.library_field_genres()}
							value={form.genres}
							onChange={set("genres")}
							help={m.library_field_genres_help()}
						/>
						<Field
							id="tags"
							label={m.library_field_tags()}
							value={form.tags}
							onChange={set("tags")}
							help={m.library_field_tags_help()}
						/>
					</fieldset>
					{/* The host's copy could not be read: the hint fields above are blank, and
					    neither they nor `prep` are sent, so the host keeps what it has. */}
					{mode === "edit" && !form.hintsLoaded && (
						<p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm">
							{m.library_edit_hints_unread()}
						</p>
					)}
					{error && (
						<p
							role="alert"
							className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
						>
							{error}
						</p>
					)}
					<div className="flex gap-2">
						<Button type="submit" disabled={isSaving || !form.title.trim()}>
							{mode === "edit" ? m.library_save() : m.library_create()}
						</Button>
						<Button type="button" variant="outline" onClick={onCancel}>
							{m.library_cancel()}
						</Button>
					</div>
				</form>
			</CardContent>
		</Card>
	);
};
