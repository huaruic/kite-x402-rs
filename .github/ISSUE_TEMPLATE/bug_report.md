---
name: Bug report
about: Something behaves differently from the documented contract
labels: bug
---

**What happened**

**What you expected**
<!-- If it differs from the TypeScript or Go template, say which and how. -->

**Reproduction**
<!-- The request, and the relevant configuration with secrets redacted. -->

```
KITE_NETWORK=
PRICE_USD=
UPSTREAM_URL=
```

```bash
curl -i "localhost:8080/v1/..."
```

**Response**
<!-- Status line, and the decoded PAYMENT-REQUIRED / PAYMENT-RESPONSE header if
     one was present. Never paste PAYMENT-SIGNATURE: it is a signed
     authorization. -->

**Environment**

- `rustc --version`:
- Deployment: local / Docker / Fly / Render / other
