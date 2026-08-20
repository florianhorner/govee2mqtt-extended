# Govee live 2FA test

- **Result:** FAIL
- **Reason:** `email_wait_timed_out`
- **Commit:** `b11264d649cb` (prefix; full SHA in manifest)
- **Govee/IMAP contacted:** yes
- **Login requests:** 1/6
- **Govee HTTP request upper bound:** 10 (6 login + 4 verification)
- **Email-trigger request upper bound:** 4 (passing run observes 2 emails)
- **Matching emails observed:** 0/2
- **Device hardware exercised:** no
- **Secrets or message contents retained:** no
