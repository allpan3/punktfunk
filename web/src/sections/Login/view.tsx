import { ease } from "@unom/style";
import { motion } from "motion/react";
import type { FC } from "react";
import Logo from "@/components/logo";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { m } from "@/paraglide/messages";

export const LoginView: FC<{
	onSubmit: (password: string) => void;
	error: boolean;
	busy: boolean;
}> = ({ onSubmit, error, busy }) => {
	return (
		<div className="flex flex-col min-h-screen items-center justify-center p-6">
			<motion.div
				transition={ease.quint(0.9).out}
				variants={{ enter: { scale: 1, y: 0 }, from: { scale: 0, y: 100 } }}
				className="mb-8 flex w-[120px]"
			>
				<Logo />
			</motion.div>
			<Card className="w-full max-w-sm h-fit grow-0">
				<CardHeader className="items-start text-left">
					<CardTitle className="text-xl">{m.login_title()}</CardTitle>
					<p className="text-sm text-muted-foreground">
						{m.login_subtitle()}{" "}
						<a
							href="https://docs.punktfunk.unom.io/docs/forgot-password"
							target="_blank"
							rel="noreferrer"
							className="underline underline-offset-4 hover:text-foreground"
						>
							{m.login_docs_link()}
						</a>
					</p>
				</CardHeader>
				<CardContent>
					{/* Uncontrolled: a password typed or autofilled before hydration never
					    reaches React state, so read the field itself and let `required` gate. */}
					<form
						onSubmit={(e) => {
							e.preventDefault();
							const data = new FormData(e.currentTarget);
							onSubmit(String(data.get("password") ?? ""));
						}}
						className="space-y-4"
					>
						<div className="space-y-2">
							<Label htmlFor="pw">{m.login_password()}</Label>
							<Input
								id="pw"
								name="password"
								type="password"
								autoFocus
								required
								autoComplete="current-password"
							/>
						</div>
						{error && (
							<p className="text-sm text-destructive">{m.login_error()}</p>
						)}
						<Button type="submit" className="w-full" disabled={busy}>
							{busy ? m.login_signing_in() : m.login_submit()}
						</Button>
					</form>
				</CardContent>
			</Card>
		</div>
	);
};
