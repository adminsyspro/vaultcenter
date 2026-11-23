# Email notifications

Administrators configure **Administration → Notifications** (`/#notifications`).
The configuration is shared by all VTLs. Each email identifies the library
and the operation concerned. Notifications are disabled at installation.

## Configuration

1. Enter the SMTP server and its port, then choose STARTTLS required
   (usually 587) or Implicit TLS (usually 465). Certificates are verified.
2. Enter the username and password if the relay requires authentication.
   A password field left empty keeps the saved secret. The delete checkbox
   clears it explicitly; also clear the username if needed.
3. Enter the sender, the installation name and up to 20 recipients,
   one address per line. Use standard ASCII addresses.
4. Choose the successes and failures to receive, enable, then save.
5. Use **Send test email**, then check the log and the mailbox.
   The test uses all saved recipients, at most once per minute.

The preview uses the saved configuration. An editing conflict requires a
reload before saving. The password is never returned by the API.
The links in the emails use `COLD_VTL_PUBLIC_URL` and open the details of the
operation after authentication, even if it is no longer in the recent list.

## Events

By default, successes and failures are selected for S3 export, S3 restore,
PBS reintegration, archiving and recall. Archiving and recall are reported
as complete only after final confirmation from the provider. An accepted request
is not enough. The failure filter includes uncertain results; preparation
failures are also reported. The end of a native PBS copy to a tape is not
inferred from the drive animation.

Past events are not picked up on activation. Each event keeps the recipients
it had when it was created. Changing the list does not redirect emails that
are already queued. Disabling suspends new sends and the capture of new
events; a send that has already started may finish. Re-enabling resumes the
queued messages with their original recipients.

## Log and retries

- **Queued**: message ready or new attempt scheduled.
- **Running**: SMTP transaction in progress.
- **Accepted by relay**: positive response from the SMTP relay, with no
  guarantee of final delivery or of placement outside the spam folder.
- **Failed**: permanent error or five attempts exhausted.
- **Delivery uncertain**: interruption while the relay may have accepted the
  message. No automatic resend, to limit duplicates.

Errors that are certainly temporary are retried after 1, 5, 15 then 60 minutes,
with five attempts in total. Sends interrupted by a restart become uncertain.
Check the relay logs and the recipient's mailbox before any manual action;
there is no button to resend an event. Accepted emails are kept for 30 days
in the log. Failures and uncertain deliveries remain available. An SMTP outage
does not change the result of the backup job.

## Installation and operation

The Docker image installs the SMTP program automatically. For a native
installation, from the repository root:

```sh
sudo install -m 0755 deploy/cold-vtl/notify-email.py /usr/local/libexec/vaultcenter-notify-email
```

Python 3 with its standard library and the system certificate authorities
are required. The path can be overridden with `COLD_VTL_EMAIL_HELPER`. The
program receives its data on standard input, never in the process arguments.
The dispatcher is built into the API: run a single API instance per catalogue.

The `0014_notifications.sql` migration creates the settings, events and
deliveries. Back up the catalogue before upgrading. It contains the SMTP
secret, which is not encrypted in SQLite. Protect the data directory and its
backups with the same access restrictions as the other secrets of the
appliance. The emails do not contain raw errors, configurations or secrets.

The template contains a text version and an HTML version with inline styles,
with the VaultCenter logo embedded as an inline MIME part (Content-ID),
with no remote resource, script or open tracking. Use **Send test email** to
check the rendering in the mail clients you use, on desktop and mobile.
