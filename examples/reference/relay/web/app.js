// The Relay web client: one room, live presence and messages, over WebTransport when the
// browser and the network allow it, else over the RPC WebSocket (the frames are the same:
// design note §5). Settings come from the URL fragment, which never reaches a server:
//
//   /#token=<JWT>&room=<room>&text=<message to post>&expect=<text to wait for>
//     [&hash=<certificate SHA-256, hex: serverCertificateHashes>]
//     [&wt=<WebTransport URL>][&ws=<WebSocket URL>]
//
// Browsers cannot send an Authorization header on WebSocket or WebTransport, so the token goes
// in the `access_token` query parameter of those two URLs (the server moves it into the
// header). What the page saw is in `window.__relay` (the browser test reads it).
"use strict";

const enc = new TextEncoder();
const dec = new TextDecoder();
const params = new URLSearchParams(location.hash.slice(1));
const report = (window.__relay = {
	state: "starting",
	members: [],
	messages: [],
});

function show(text) {
	const status = document.getElementById("status");
	if (status) status.textContent = text;
}

function withToken(url) {
	const u = new URL(url);
	u.searchParams.set("access_token", params.get("token") || "");
	return u.toString();
}

/// A u32-BE length-prefixed record of one JSON frame (WebTransport streams).
function record(frame) {
	const bytes = enc.encode(JSON.stringify(frame));
	const out = new Uint8Array(4 + bytes.length);
	new DataView(out.buffer).setUint32(0, bytes.length);
	out.set(bytes, 4);
	return out;
}

/// The frames of a WebTransport stream, one per call of the returned function (null at end).
function records(readable) {
	const reader = readable.getReader();
	let buf = new Uint8Array(0);
	return async function next() {
		for (;;) {
			if (buf.length >= 4) {
				const n = new DataView(buf.buffer, buf.byteOffset).getUint32(0);
				if (buf.length >= 4 + n) {
					const frame = JSON.parse(dec.decode(buf.slice(4, 4 + n)));
					buf = buf.slice(4 + n);
					return frame;
				}
			}
			const { value, done } = await reader.read();
			if (done) return null;
			const grown = new Uint8Array(buf.length + value.length);
			grown.set(buf);
			grown.set(value, buf.length);
			buf = grown;
		}
	};
}

/// A queue of frames, for a call on the multiplexed WebSocket.
function mailbox() {
	const items = [];
	const waiting = [];
	return {
		push(frame) {
			const w = waiting.shift();
			if (w) w(frame);
			else items.push(frame);
		},
		next() {
			if (items.length) return Promise.resolve(items.shift());
			return new Promise((resolve) => waiting.push(resolve));
		},
	};
}

function timeout(ms, what) {
	return new Promise((_, reject) =>
		setTimeout(() => reject(new Error(what + " timed out")), ms),
	);
}

/// WebTransport: a control stream (ping, then the server's hello), then one stream per call.
async function overWebTransport() {
	if (typeof WebTransport === "undefined")
		throw new Error("no WebTransport in this browser");
	const url = params.get("wt") || `https://${location.host}/rpc/wt`;
	const options = {};
	const hash = params.get("hash");
	if (hash) {
		const value = new Uint8Array(hash.match(/../g).map((h) => parseInt(h, 16)));
		options.serverCertificateHashes = [{ algorithm: "sha-256", value }];
	}
	const session = new WebTransport(withToken(url), options);
	await Promise.race([session.ready, timeout(8000, "webtransport")]);
	const control = await session.createBidirectionalStream();
	await control.writable.getWriter().write(record({ id: 0, kind: "ping" }));
	const hello = await records(control.readable)();
	if (!hello || hello.kind !== "hello")
		throw new Error("no hello on the control stream");
	return {
		name: "webtransport",
		async open(call) {
			const stream = await session.createBidirectionalStream();
			const writer = stream.writable.getWriter();
			await writer.write(record(call));
			const next = records(stream.readable);
			return { send: (frame) => writer.write(record(frame)), next };
		},
	};
}

/// The RPC WebSocket: the server's hello, then every call multiplexed by id.
function overWebSocket() {
	const url = params.get("ws") || `wss://${location.host}/rpc/ws`;
	return new Promise((resolve, reject) => {
		const socket = new WebSocket(withToken(url), "idrpc.json.v1");
		const calls = new Map();
		socket.onerror = () => reject(new Error("websocket error"));
		socket.onmessage = (event) => {
			const frame = JSON.parse(event.data);
			if (frame.kind === "hello") {
				resolve({
					name: "websocket",
					async open(call) {
						const box = mailbox();
						calls.set(call.id, box);
						socket.send(JSON.stringify(call));
						return {
							send: (f) => socket.send(JSON.stringify(f)),
							next: () => box.next(),
						};
					},
				});
				return;
			}
			const box = calls.get(frame.id);
			if (box) box.push(frame);
		};
	});
}

/// The call's next item or exit (credit frames are for the transport, not the page).
async function item(call) {
	for (;;) {
		const frame = await call.next();
		if (!frame || frame.kind !== "credit") return frame;
	}
}

async function connect() {
	try {
		return { transport: await overWebTransport(), why: null };
	} catch (error) {
		return { transport: await overWebSocket(), why: String(error) };
	}
}

async function main() {
	const room = params.get("room") || "general";
	const { transport, why } = await connect();
	report.transport = transport.name;
	if (why) report.why = why;
	show(`connected over ${transport.name}`);

	// Enter the room: who is there, then live presence and messages.
	const join = await transport.open({
		id: 1,
		kind: "call",
		tag: "Relay.join",
		v: 1,
		payload: { room, status: "online" },
	});
	const first = await item(join);
	if (!first || first.kind !== "item" || first.payload.kind !== "joined") {
		throw new Error("join refused: " + JSON.stringify(first));
	}
	report.members = first.payload.members.map((m) => m.user);
	report.state = "joined";

	// Post over the two-way stream: one acknowledgement per message.
	const chat = await transport.open({
		id: 2,
		kind: "call",
		tag: "Relay.chat",
		v: 1,
		payload: null,
	});
	const text = params.get("text") || "hello";
	await chat.send({ id: 2, kind: "item", payload: { room, text } });
	const ack = await item(chat);
	if (!ack || ack.kind !== "item")
		throw new Error("no acknowledgement: " + JSON.stringify(ack));
	report.ack = ack.payload;

	// Wait for our message and the one we expect from elsewhere to come back through the room.
	const wanted = new Set([text, params.get("expect")].filter(Boolean));
	const list = document.getElementById("messages");
	while (wanted.size) {
		const frame = await item(join);
		if (!frame || frame.kind === "exit")
			throw new Error("the room closed: " + JSON.stringify(frame));
		const event = frame.payload;
		if (event.kind === "message" && event.message) {
			report.messages.push(event.message);
			wanted.delete(event.message.text);
			const li = document.createElement("li");
			li.textContent = `${event.message.from}@${event.message.node}: ${event.message.text}`;
			if (list) list.append(li);
		}
	}
	report.state = "done";
	show("done");
}

main().catch((error) => {
	report.state = "failed";
	report.error = String(error);
	show(String(error));
});
