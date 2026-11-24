#!/usr/bin/env python3
"""Loopback proxy and fake OAuth issuer for the ModelCat development preview."""

from base64 import urlsafe_b64encode
from hashlib import sha256
from html import escape
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import re
import secrets
from threading import Lock
from urllib.parse import parse_qs, urlencode, urlsplit


ISSUER_HOST = "claude-oauth.fixture.invalid"
REDIRECT_HOST = "claude-redirect.fixture.invalid"
CALLBACK = re.compile(r"http://127\.0\.0\.1:19701/oauth-preview/[a-f0-9]{32}/callback")


class PreviewIssuer(BaseHTTPRequestHandler):
    timeout = 5
    lock = Lock()
    grant = None
    access_token = None
    refresh_token = None

    def log_message(self, *_):
        pass

    def send_json(self, status, payload):
        self.send_body(status, "application/json", json.dumps(payload))

    def send_body(self, status, content_type, body):
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Referrer-Policy", "no-referrer")
        self.end_headers()
        self.wfile.write(data)

    def redirect(self, location):
        self.send_response(303)
        self.send_header("Location", location)
        self.send_header("Content-Length", "0")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Referrer-Policy", "no-referrer")
        self.end_headers()

    def target(self):
        url = urlsplit(self.path)
        if url.scheme == "http" and url.netloc in (ISSUER_HOST, REDIRECT_HOST):
            return url
        return None

    def record(self, event):
        print(event, flush=True)

    def do_GET(self):
        url = self.target()
        if url is None:
            self.send_json(404, {"error": "unknown_target"})
            return
        params = {key: values[0] for key, values in parse_qs(url.query).items()}
        if url.netloc == REDIRECT_HOST:
            with self.lock:
                grant = type(self).grant
                if url.path == "/complete" and grant and grant.get("ticket") and params.get("ticket") == grant["ticket"]:
                    grant.pop("ticket")
                else:
                    grant = None
            if grant is None:
                self.send_json(400, {"error": "invalid_redirect"})
                return
            self.record("browser redirect")
            response = urlencode({"state": grant["state"], "code": grant["code"]})
            self.redirect(f"{grant['redirect_uri']}?{response}")
            return
        if url.path == "/ready":
            self.send_json(200, {"ready": True})
            return
        if (
            url.path not in ("/authorize", "/approve")
            or params.get("client_id") != "modelcat-oauth-preview"
            or params.get("response_type") != "code"
            or params.get("code_challenge_method") != "S256"
            or not params.get("state")
            or not params.get("code_challenge")
            or not CALLBACK.fullmatch(params.get("redirect_uri", ""))
        ):
            self.send_json(400, {"error": "invalid_authorization"})
            return
        if url.path == "/authorize":
            self.record("browser authorization")
            fields = "".join(
                f'<input type="hidden" name="{key}" value="{escape(params[key], quote=True)}">'
                for key in ("client_id", "response_type", "code_challenge_method",
                            "state", "code_challenge", "redirect_uri")
            )
            self.send_body(200, "text/html; charset=utf-8", (
                '<!doctype html><html lang="zh-CN"><meta charset="utf-8">'
                '<meta name="viewport" content="width=device-width, initial-scale=1">'
                '<title>Claude 模拟授权</title>'
                '<style>html{color-scheme:light dark;font:16px system-ui}'
                'main{max-width:34rem;margin:5rem auto;padding:1.25rem}'
                'h1{font-size:1.5rem}button{font:inherit;padding:.4rem .8rem}</style>'
                '<main><h1>Claude 模拟授权</h1>'
                f'<form action="/approve" method="get">{fields}'
                '<button type="submit">确认模拟授权</button></form></main></html>'
            ))
            return
        params["code"] = secrets.token_urlsafe(24)
        params["ticket"] = secrets.token_urlsafe(24)
        with self.lock:
            type(self).grant = params
        self.record("browser approval")
        self.redirect(f"http://{REDIRECT_HOST}/complete?{urlencode({'ticket': params['ticket']})}")

    def do_POST(self):
        url = self.target()
        length = self.headers.get("Content-Length", "")
        if url is None or url.netloc != ISSUER_HOST or not length.isdecimal() or int(length) > 262144:
            self.send_json(400, {"error": "invalid_request"})
            return
        try:
            body = json.loads(self.rfile.read(int(length)))
        except (ValueError, UnicodeError):
            self.send_json(400, {"error": "invalid_request"})
            return
        if not isinstance(body, dict):
            self.send_json(400, {"error": "invalid_request"})
            return
        if url.path in ("/v1/messages", "/v1/messages/count_tokens"):
            self.inference(url.path, body)
            return
        if url.path != "/token" or body.get("client_id") != "modelcat-oauth-preview":
            self.send_json(400, {"error": "invalid_client"})
            return
        kind = body.get("grant_type")
        tokens = self.issue_token(kind, body)
        if tokens is None:
            self.send_json(400, {"error": "invalid_grant"})
            return
        self.record("node token exchange" if kind == "authorization_code" else "node token refresh")
        self.send_json(200, tokens)

    def issue_token(self, kind, body):
        issuer = type(self)
        with self.lock:
            if kind == "authorization_code":
                grant = issuer.grant
                verifier = body.get("code_verifier")
                proof = urlsafe_b64encode(sha256(verifier.encode()).digest()).decode().rstrip("=") if isinstance(verifier, str) else None
                if not grant or "ticket" in grant or proof != grant["code_challenge"] or any(
                    body.get(field) != grant[field] for field in ("code", "state", "redirect_uri")
                ):
                    return None
                issuer.grant = None
            elif kind != "refresh_token" or not issuer.refresh_token or body.get("refresh_token") != issuer.refresh_token:
                return None
            issuer.access_token = "preview-access-" + secrets.token_urlsafe(24)
            issuer.refresh_token = "preview-refresh-" + secrets.token_urlsafe(24)
            return {
                "access_token": issuer.access_token,
                "refresh_token": issuer.refresh_token,
                "expires_in": 300,
            }

    def inference(self, path, request):
        with self.lock:
            access_token = type(self).access_token
        if not access_token or not secrets.compare_digest(
            self.headers.get("Authorization", "").encode(), f"Bearer {access_token}".encode()
        ):
            self.send_json(401, {"error": "invalid_token"})
            return
        if request.get("model") != "claude-oauth-preview":
            self.send_json(400, {"error": "invalid_model"})
            return
        if path.endswith("/count_tokens"):
            self.send_json(200, {"input_tokens": 16})
            return
        self.record("node token-backed inference")
        answer = "ModelCat OAuth preview OK."
        message = {
            "id": "msg_modelcat_oauth_preview", "type": "message", "role": "assistant",
            "model": "claude-oauth-preview", "content": [], "stop_reason": None,
            "stop_sequence": None, "usage": {"input_tokens": 16, "output_tokens": 0},
        }
        if request.get("stream") is not True:
            message["content"] = [{"type": "text", "text": answer}]
            message["stop_reason"] = "end_turn"
            message["usage"]["output_tokens"] = 8
            self.send_json(200, message)
            return
        events = (
            {"type": "message_start", "message": message},
            {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
            {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": answer}},
            {"type": "content_block_stop", "index": 0},
            {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None},
             "usage": {"output_tokens": 8}},
            {"type": "message_stop"},
        )
        stream = "".join(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n" for event in events)
        self.send_body(200, "text/event-stream", stream)


with ThreadingHTTPServer(("127.0.0.1", 18779), PreviewIssuer) as server:
    server.serve_forever()
