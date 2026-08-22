# Govee live 2FA test

- **Result:** FAIL
- **Reason:** `unexpected_email_during_quiet_window`
- **Commit:** `9fffa49e5aee` (prefix; full SHA in manifest)
- **Govee/IMAP contacted:** yes
- **Login requests:** 2/6
- **Govee HTTP request upper bound:** 10 (6 login + 4 verification)
- **Email-trigger request upper bound:** 4 (passing run observes 2 emails)
- **Matching emails observed:** 2/2
- **Device hardware exercised:** no
- **Secrets or message contents retained:** no
