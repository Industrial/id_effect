// The generated TypeScript client (`web/rpc`) against a running Ledger (tests/rpc.rs,
// `ledger_ts_client_round_trip`): `deno test … -- <rpc url> <owner token> <viewer cookie>`.
// No remote imports: the assertions are the few this file needs.

import { LedgerV1Client, RpcProblemError } from "../../web/rpc/client.ts";

const [url, ownerToken, viewerCookie] = Deno.args;

function assert(condition: unknown, message: string): asserts condition {
	if (!condition) throw new Error(message);
}

function client(headers: Record<string, string>): LedgerV1Client {
	return new LedgerV1Client({ url, headers: () => Promise.resolve(headers) });
}

Deno.test("the Ledger over RPC from TypeScript", async () => {
	assert(url && ownerToken && viewerCookie, "run through tests/rpc.rs");
	// The owner with an API token; the viewer with its session cookie (no CSRF token: C5).
	const owner = client({ Authorization: `Bearer ${ownerToken}` });

	// Two accounts, then an entry between them; the same key replays the posting.
	const cash = await owner.open_account({
		code: "1100",
		name: "Cash",
		kind: "asset",
	});
	assert(cash._tag === "Success", JSON.stringify(cash));
	const sales = await owner.open_account({
		code: "4100",
		name: "Sales",
		kind: "income",
	});
	assert(sales._tag === "Success", JSON.stringify(sales));
	const entry = {
		memo: "a sale",
		lines: [
			{ account_id: cash.value.id, amount: 1250 },
			{ account_id: sales.value.id, amount: -1250 },
		],
	};
	const key = "ts-round-trip-0001";
	const posted = await owner.post_entry(entry, { idempotencyKey: key });
	assert(posted._tag === "Success", JSON.stringify(posted));
	const again = await owner.post_entry(entry, { idempotencyKey: key });
	assert(
		again._tag === "Success" && again.value.id === posted.value.id,
		`a replayed key answers the first posting: ${JSON.stringify(again)}`,
	);

	// Reads: the entry, the balance, the list (one batch request for the three).
	const [read, balance, accounts] = await Promise.all([
		owner.entry(posted.value.id),
		owner.balance(cash.value.id),
		owner.accounts(),
	]);
	assert(
		read._tag === "Success" &&
			read.value.lines.length === 2 &&
			read.value.memo === "a sale",
		JSON.stringify(read),
	);
	assert(
		balance._tag === "Success" &&
			balance.value.balance === 1250 &&
			balance.value.postings === 1,
		JSON.stringify(balance),
	);
	assert(
		accounts._tag === "Success" && accounts.value.accounts.length === 2,
		JSON.stringify(accounts),
	);

	// Typed errors: what the REST Problem says.
	const missing = await owner.account(999_999);
	assert(
		missing._tag === "Failure" &&
			missing.error._tag === "NotFound" &&
			missing.error.detail === "no such account",
		JSON.stringify(missing),
	);
	const unbalanced = await owner.post_entry({
		memo: "unbalanced",
		lines: [
			{ account_id: cash.value.id, amount: 1 },
			{ account_id: sales.value.id, amount: 2 },
		],
	});
	assert(
		unbalanced._tag === "Failure" && unbalanced.error._tag === "InvalidInput",
		JSON.stringify(unbalanced),
	);

	// The guards: a viewer may read, not post; nobody signed in may do neither.
	const viewer = client({ Cookie: viewerCookie });
	assert((await viewer.accounts())._tag === "Success", "a viewer reads");
	let refused: unknown;
	try {
		await viewer.post_entry(entry);
	} catch (e) {
		refused = e;
	}
	assert(
		refused instanceof RpcProblemError && refused.status === 403,
		`a viewer may not post: ${refused}`,
	);
	let anonymous: unknown;
	try {
		await new LedgerV1Client({ url }).accounts();
	} catch (e) {
		anonymous = e;
	}
	assert(
		anonymous instanceof RpcProblemError && anonymous.status === 401,
		`sign in first: ${anonymous}`,
	);
	owner.connection.close();
	viewer.connection.close();
});
