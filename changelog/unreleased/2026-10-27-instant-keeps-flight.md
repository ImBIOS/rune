- **An instant commit no longer lands motion it didn't change.** `Tx::instant` still lands
  every layout, colour and opacity change the commit makes, but a node already springing toward
  a place or value the commit leaves alone keeps springing, so a quick key press during a layout
  spring (a row of chips opening, a selection moving) doesn't snap it to the end mid-flight.
  `Tx::settle` still lands every running track; use it where an instant commit was used to stop
  motion in place.
