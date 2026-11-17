"""A fake AWS STS for the MONGODB-AWS integration tests (tests/aws.rs).

mongod forwards the client's signed GetCallerIdentity request here instead of
to AWS (`awsSTSUrl`). The access key ID in the signature decides the answer,
so a test can tell which credentials nordpil used. Signatures aren't checked.
"""

import http.server
import ssl

ACCOUNT = "123456789012"
# Access key ID -> (IAM user, session token the request must carry).
KEYS = {
    "AKIDALPHA": ("alpha", None),
    "AKIDBETA": ("beta", None),
    "AKIDTOKEN": ("token", "session-token-1"),
}


class Sts(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        auth = self.headers.get("Authorization", "")
        key = auth.split("Credential=", 1)[-1].split("/", 1)[0]
        user, token = KEYS.get(key, (None, None))
        if user is None or token != self.headers.get("X-Amz-Security-Token"):
            self.reply(403, "<ErrorResponse><Error><Code>InvalidClientTokenId</Code>"
                            "</Error></ErrorResponse>")
            return
        self.reply(200,
            '<GetCallerIdentityResponse xmlns="https://sts.amazonaws.com/doc/2011-06-15/">'
            f"<GetCallerIdentityResult><Arn>arn:aws:iam::{ACCOUNT}:user/{user}</Arn>"
            f"<UserId>AIDA{user.upper()}</UserId><Account>{ACCOUNT}</Account>"
            "</GetCallerIdentityResult><ResponseMetadata><RequestId>test</RequestId>"
            "</ResponseMetadata></GetCallerIdentityResponse>")

    def reply(self, status, body):
        data = body.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/xml")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


server = http.server.HTTPServer(("0.0.0.0", 8443), Sts)
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain("/app/sts.pem", "/app/sts.key")
server.socket = context.wrap_socket(server.socket, server_side=True)
print("mock-sts ready", flush=True)
server.serve_forever()
