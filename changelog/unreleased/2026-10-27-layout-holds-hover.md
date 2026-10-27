- **Hover waits while layout moves content under a still pointer.** When a row expanded and
  the rows below slid, each row passing under a pointer that hadn't moved took the hover for a
  frame or so, flashing its hover-revealed controls. The glide hold now covers all layout
  motion: frame and offset springs, growth stacks, growths, flights and app-asked glides. As on
  macOS, hover lands on the row under the pointer once the motion settles, or at once when the
  pointer moves or presses. The hovered element, `on_hover_change`, hover styles, the cursor
  and tooltips all hold together. A wheel or trackpad scroll still hovers each row it brings
  under the pointer, a press still shows and lets go, and an element that leaves the tree,
  starts its exit or turns inert (a modal opening) lets go of hover at once. An element on its
  way out now hears `on_hover_change(false)`, which it used to miss; with `hover_delays`, only
  an element whose enter was reported hears the leave, now also when it unmounts. A held press
  no longer keeps every element it passed inside: only what contains the pressed element.
