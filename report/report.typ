// Student report template.
//
// All data arrives as JSON in sys.inputs.data (built by src/report.rs), so
// names and comments are always treated as plain text, never as markup.
// The optional logo arrives as bytes in sys.inputs.logo.

#let d = json(bytes(sys.inputs.data))
// Colours come ready-made from src/color.rs: banners in the main school
// colour, trim stripes in the second, and tints of the main colour behind
// text, so text never sits on a colour it can't be read on.
#let c = d.colors
#let primary = rgb(c.banner)
#let on-primary = rgb(c.on_banner)
#let trim = rgb(c.trim)
#let rule = rgb(c.rule)
#let primary-ink = rgb(c.heading)
#let muted = rgb(c.muted)
#let ink = rgb(c.ink)
#let head-fill = rgb(c.head_fill)
#let group-fill = rgb(c.group_fill)
#let pale = rgb(c.pale)
#let hairline = rgb(c.line)

#set document(title: d.student.name + " – " + d.title)
#set text(font: "Source Sans 3", size: 9.5pt, fill: ink, lang: "en")
#set par(leading: 0.55em)

#set page(
  paper: "us-letter",
  margin: (top: 1.05in, bottom: 0.75in, x: 0.6in),
  background: context {
    // Header banner
    place(top + left, rect(width: 100%, height: 0.75in, fill: primary))
    place(top + left, dy: 0.75in, rect(width: 100%, height: 0.05in, fill: trim))
    if d.has_logo {
      place(top + left, dx: 0.4in, dy: 0.125in, box(height: 0.5in, image(sys.inputs.logo, height: 0.5in)))
    }
    place(top + center, dy: 0.16in, align(center)[
      #text(fill: on-primary, size: 15pt, weight: "bold", d.school)
      #v(-0.55em)
      #text(fill: on-primary, size: 9pt, d.subtitle)
    ])
    // Footer banner
    place(bottom + left, rect(width: 100%, height: 0.45in, fill: primary))
    place(bottom + left, dy: -0.45in, rect(width: 100%, height: 0.05in, fill: trim))
    let ft = (body) => text(fill: on-primary, size: 7.5pt, body)
    place(bottom + left, dx: 0.5in, dy: -0.17in, ft("Generated " + d.date))
    place(bottom + center, dy: -0.17in, ft(d.footer))
    let total = counter(page).final().first()
    place(bottom + right, dx: -0.5in, dy: -0.17in, ft("Page " + str(here().page()) + " of " + str(total)))
  },
)

// ── Helpers ──────────────────────────────────────────────────────────────────

#let bar(frac, color, width: 1.3in) = box(
  width: width, height: 6pt, fill: pale, stroke: 0.4pt + hairline, radius: 1pt, clip: true,
  if frac > 0 { place(left, rect(width: calc.min(frac, 1.0) * 100%, height: 6pt, fill: rgb(color), radius: 1pt)) },
)

#let graded(txt, color, ..args) = text(fill: rgb(color), weight: "bold", ..args, txt)

#let banner(left-part, right-part: none, fill: primary) = block(
  width: 100%, fill: fill, inset: (x: 8pt, y: 5pt), radius: (top: 3pt), below: 0pt,
  grid(columns: (1fr, auto), align: (left + horizon, right + horizon),
    text(fill: on-primary, weight: "bold", size: 11pt, left-part),
    if right-part != none { text(fill: on-primary, size: 9.5pt, right-part) },
  ),
)

#let hdr(body) = text(fill: primary-ink, weight: "bold", size: 8pt, body)

// ── Student identity ─────────────────────────────────────────────────────────

#grid(columns: (1fr, auto), align: (left + bottom, right + bottom),
  text(size: 20pt, weight: "bold", fill: primary-ink, d.student.name),
  text(size: 8.5pt, fill: muted)[
    #for (i, line) in d.meta_lines.enumerate() {
      if i > 0 { linebreak() }
      line
    }
  ],
)
#v(2pt)
#line(length: 100%, stroke: 2pt + rule)
#v(4pt)

// ── Summary ──────────────────────────────────────────────────────────────────

#if d.subjects.len() == 1 {
  let s = d.subjects.at(0)
  block(width: 100%, fill: pale, stroke: 1.5pt + primary, radius: 4pt, inset: 8pt,
    grid(columns: (1.4fr, 1fr, 1fr, 1.1fr, 0.8fr), align: center + horizon, column-gutter: 4pt,
      align(left, text(fill: primary-ink, weight: "bold", size: 10pt, if s.overridden { "Final Grade" } else { "Current Grade" })),
      graded(s.overall_pct, s.overall_color, size: 22pt),
      graded(s.overall_label, s.overall_color, size: 22pt),
      text(fill: muted, size: 9pt)[Assessments#linebreak()Submitted],
      text(fill: primary-ink, weight: "bold", size: 16pt, s.submitted),
    ),
  )
} else if d.subjects.len() > 1 {
  block(width: 100%, stroke: 1.5pt + primary, radius: 4pt, clip: true,
    table(
      columns: (1fr, auto, auto, auto, 1.4in),
      stroke: none,
      fill: (_, y) => if y == 0 { head-fill } else if calc.even(y) { pale } else { white },
      inset: (x: 7pt, y: 5pt),
      align: (left + horizon, center + horizon, center + horizon, center + horizon, center + horizon),
      hdr[Subject], hdr[Grade], hdr[], hdr[Submitted], hdr[],
      ..d.subjects.map(s => (
        text(weight: "bold", s.name),
        graded(if s.style == "letter" { s.overall_label } else { s.overall_pct }, s.overall_color, size: 10.5pt),
        if s.style == "letter" { [] } else { graded(s.overall_label, s.overall_color) },
        text(fill: muted, s.submitted),
        bar(s.overall_frac, s.overall_color),
      )).flatten(),
    ),
  )
}

#if d.options.show_legend {
  v(6pt)
  let n = d.legend.len()
  let cols = if n > 7 { calc.ceil(n / 2) } else { n }
  block(width: 100%, fill: pale, stroke: 0.5pt + hairline, radius: 2pt, inset: 4pt,
    grid(columns: (1fr,) * cols, row-gutter: 3pt, align: center,
      ..d.legend.map(b => text(size: 7.5pt)[#text(fill: rgb(b.color), weight: "bold", b.label) #h(2pt) #b.range]),
    ),
  )
}

#v(8pt)

// ── Subjects ─────────────────────────────────────────────────────────────────

#for s in d.subjects {
  let headline = if s.overall_pct == "—" { "No marks yet" } else if s.style == "letter" { "Grade: " + s.overall_label } else { "Overall: " + s.overall_pct + "  (" + s.overall_label + ")" }
  // Status line
  let status = (s.submitted + " submitted",)
  if s.missing > 0 { status.push(str(s.missing) + " missing") }
  if s.pending > 0 { status.push(str(s.pending) + " awaiting marking") }
  if s.excused > 0 { status.push(str(s.excused) + " excused") }

  block(breakable: false, below: 0pt)[
    #banner(s.name, right-part: headline)
    #block(width: 100%, fill: pale, inset: (x: 8pt, y: 4pt), below: 0pt, stroke: (x: 1pt + primary),
      text(size: 8.5pt, fill: muted, status.join("  ·  ") + if s.overridden { "  ·  Final grade set by teacher" } else { "" }))
  ]

  // Categories
  if d.options.show_categories and s.categories.len() > 0 {
    table(
      columns: (1fr, auto, auto, auto, 1.5in),
      stroke: (x, y) => (x: none, bottom: 0.4pt + hairline),
      fill: (_, y) => if y == 0 { head-fill } else { white },
      inset: (x: 7pt, y: 4.5pt),
      align: (left + horizon, center + horizon, center + horizon, center + horizon, center + horizon),
      table.hline(stroke: 1pt + primary),
      hdr[Category], hdr[Weight], hdr[Average], hdr[Grade], hdr[],
      ..s.categories.map(c => (
        c.name,
        text(fill: muted, c.weight),
        graded(c.pct, c.color),
        graded(c.label, c.color),
        if c.frac >= 0 { bar(c.frac, c.color) } else { text(size: 7.5pt, fill: muted)[No marks yet] },
      )).flatten(),
    )
  }

  // Assessments, grouped by unit
  if d.options.show_assessments and s.units.len() > 0 {
    let rows = ()
    for u in s.units {
      if u.name != "" and s.units.len() > 1 {
        rows.push(table.cell(colspan: 4, fill: group-fill, text(weight: "bold", fill: primary-ink, u.name)))
        rows.push(table.cell(fill: group-fill, align: right, if u.avg != "—" { text(weight: "bold", fill: rgb(u.color), u.avg + " " + u.label) }))
      }
      for a in u.rows {
        rows.push(a.name)
        if a.status_text != "" {
          rows.push(table.cell(colspan: 3, align: center, text(size: 8pt, fill: rgb(a.color), style: "italic", a.status_text)))
        } else {
          rows.push(text(fill: muted, a.score))
          rows.push(graded(a.pct, a.color))
          rows.push(graded(a.label, a.color))
        }
        rows.push(if a.frac >= 0 { bar(a.frac, a.color) } else { [] })
      }
    }
    table(
      columns: (1fr, auto, auto, auto, 1.5in),
      stroke: (x, y) => (x: none, bottom: 0.4pt + hairline),
      fill: (_, y) => if y == 0 { head-fill } else if calc.even(y) { pale } else { white },
      inset: (x: 7pt, y: 4pt),
      align: (left + horizon, center + horizon, center + horizon, center + horizon, center + horizon),
      table.hline(stroke: 1pt + primary),
      table.header(hdr[Assessment], hdr[Score], hdr[%], hdr[Grade], hdr[]),
      ..rows,
    )
  }

  // Completion items (e.g. journals)
  if s.completion.len() > 0 {
    v(4pt)
    block(width: 100%, stroke: 0.5pt + hairline, radius: 2pt, inset: 6pt, below: 0pt)[
      #text(weight: "bold", fill: primary-ink)[#s.completion_title]
      #h(6pt) #text(size: 8.5pt, fill: muted, s.completion_summary)
      #v(-2pt)
      #grid(columns: (1fr,) * 4, row-gutter: 4pt, column-gutter: 6pt,
        ..s.completion.map(c => text(size: 8.5pt)[
          #box(width: 7pt, height: 7pt, radius: 1pt, fill: if c.done { rgb("#1A7340") } else { white }, stroke: 0.6pt + if c.done { rgb("#1A7340") } else { rgb("#B22222") })
          #h(2pt) #c.name
        ]),
      )
    ]
  }

  // Outstanding work
  if d.options.show_missing and s.missing_list.len() > 0 {
    v(4pt)
    block(width: 100%, fill: rgb("#FDF1F1"), stroke: (left: 3pt + rgb("#B22222")), inset: 6pt, below: 0pt)[
      #text(weight: "bold", fill: rgb("#B22222"))[Outstanding work: ]
      #s.missing_list.join(", ")
    ]
  }

  // Comments
  if d.options.show_comments and (s.comment != "" or s.next_steps != "") {
    v(4pt)
    block(width: 100%, stroke: 0.5pt + hairline, radius: 2pt, inset: 7pt, below: 0pt)[
      #if s.comment != "" [
        #text(weight: "bold", fill: primary-ink)[Teacher Comments] \
        #s.comment
      ]
      #if s.comment != "" and s.next_steps != "" { v(3pt) }
      #if s.next_steps != "" [
        #text(weight: "bold", fill: primary-ink)[Next Steps] \
        #s.next_steps
      ]
    ]
  }

  v(12pt)
}

// ── Learning skills ──────────────────────────────────────────────────────────

#if d.options.show_skills and d.skills.len() > 0 {
  block(breakable: false)[
    #banner("Learning Skills and Work Habits")
    #table(
      columns: (1fr, auto) * 2,
      stroke: (x, y) => (x: none, bottom: 0.4pt + hairline),
      inset: (x: 7pt, y: 4.5pt),
      align: (left, center, left, center),
      ..d.skills.map(k => (k.name, text(weight: "bold", fill: primary-ink, k.rating))).flatten(),
    )
    #text(size: 7.5pt, fill: muted)[E = Excellent · G = Good · S = Satisfactory · N = Needs Improvement]
  ]
  v(10pt)
}

// ── Closing ──────────────────────────────────────────────────────────────────

#line(length: 100%, stroke: 1pt + rule)
#align(center, text(size: 8.5pt, style: "italic", fill: muted, d.contact))

#if d.options.show_signature {
  v(22pt)
  grid(columns: (1fr, 1fr), column-gutter: 0.5in,
    [#line(length: 100%, stroke: 0.6pt + ink) #text(size: 8pt, fill: muted)[Teacher signature]],
    [#line(length: 100%, stroke: 0.6pt + ink) #text(size: 8pt, fill: muted)[Parent / guardian signature]],
  )
}
