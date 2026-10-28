---
status: accepted
---

# A person links their computers, and suggests their own nicknames

Each computer has its own key, and that key is what signs, encrypts and names everything the computer's agents do. A person with a laptop and a desktop has two keys. Until now nothing said they were one person, so a teammate saw two strangers, and a directive from the laptop to an agent on the desktop was refused as coming from someone else (ADR 0008).

A person's computers are one person when each lists the other. `robofinger link` on a computer prints a short-lived code, an invitation marked as a link. Running `robofinger link <code>` on another computer joins it to the same project and makes both computers name each other in everything they publish. Each side's claim is signed by its own key, so neither computer can claim the other alone, and either can undo the link by dropping it.

People name themselves and their computers ("John", "laptop"). Those nicknames travel with everything they publish, as suggestions. Anyone can set their own nickname for a person or a computer, and theirs wins on their screen. A suggestion that would show one person under a name you already use for someone else is ignored. The key-derived name stays beside every nickname, as the fingerprint that cannot be chosen.

## Considered Options

- **Each viewer groups keys by hand.** No protocol, but every teammate repeats the work, and directives still stop at each computer's key.
- **One key copied to every computer.** Simplest, but the record can no longer tell computers apart, and a lost laptop can only be removed by changing the person's identity for everyone.
- **Nicknames chosen only by viewers**, as labels were. Safe, but a teammate sees `lilac-viper` until they name you, and nobody learns what you call your computers.
- **Trusting the published name outright.** Then anyone could appear as anyone between one entry and the next. Suggestions are why the viewer's own names win, why colliding suggestions are dropped, and why the fingerprint never leaves the line.

## Consequences

- An agent path still starts with a computer's key-derived name, so a link means the same agent to everyone. Nicknames are for reading, never for addressing.
- A directive counts when it is signed by any computer of the agent's own person (ADR 0008).
- A link code makes whoever holds it you, until it expires. It expires in an hour unless asked otherwise.
- Each computer publishes a join when it takes up the link: the new one when it uses the code, the other when it next runs robofinger and adopts it. Until the second join, teammates still see two computers.
