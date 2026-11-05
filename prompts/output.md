When you are done, end your final message with exactly one fenced block tagged `nightporter`
containing JSON:

```nightporter
{"summary": "one or two sentences on what you did",
 "replies": [{"event": "<event id from the list above, or null for a general comment>",
              "body": "reply text, in the requested style and language"}]}
```

Write one reply per request you handled (including requests you decided not to act on, with the
reason). Mention commit hashes when you changed code. Do not include the footer; Night Porter
adds it.
