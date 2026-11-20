//! `.xlsx` export of a class: one sheet per subject. The headers use the same
//! conventions the importer reads (`Name [Unit] (/out_of)`), so an exported
//! file can be edited in Excel and imported back.

use rust_xlsxwriter::{Color, Format, FormatAlign, FormatBorder, Workbook};

use crate::{
    engine::{Kind, Scale, Status, display_mark},
    error::{AppError, AppResult},
    model::{Student, SubjectData},
};

pub fn sheet_name(name: &str, used: &mut Vec<String>) -> String {
    let clean: String = name.chars().filter(|c| !"[]:*?/\\".contains(*c)).take(28).collect();
    let base = if clean.trim().is_empty() { "Sheet".to_string() } else { clean.trim().to_string() };
    let mut candidate = base.clone();
    let mut i = 2;
    while used.iter().any(|u| u.eq_ignore_ascii_case(&candidate)) {
        candidate = format!("{base} {i}");
        i += 1;
    }
    used.push(candidate.clone());
    candidate
}

pub fn header_for(a: &crate::model::Assessment) -> String {
    let mut h = a.name.clone();
    if !a.unit.is_empty() {
        h.push_str(&format!(" [{}]", a.unit));
    }
    match Kind::parse(&a.kind) {
        Kind::Completion => h.push_str(" [done]"),
        Kind::Level => h.push_str(" [level]"),
        Kind::Scored => h.push_str(&format!(" (/{})", crate::engine::fmt_num(a.out_of))),
    }
    h
}

pub fn err(e: rust_xlsxwriter::XlsxError) -> AppError {
    AppError::Internal(anyhow::anyhow!("xlsx export: {e}"))
}

pub fn class_workbook(roster: &[Student], subjects: &[SubjectData], scale: &Scale) -> AppResult<Vec<u8>> {
    let mut wb = Workbook::new();
    let mut used = Vec::new();
    let named: Vec<(String, &SubjectData)> = subjects.iter().map(|sd| (sd.subject.name.clone(), sd)).collect();
    add_mark_sheets(&mut wb, roster, &named, scale, &mut used, false)?;
    if subjects.is_empty() {
        wb.add_worksheet();
    }
    wb.save_to_buffer().map_err(err)
}

/// One sheet of marks per subject, named as given. `locked`: protect the
/// sheet (a view of the data, not something to edit).
pub fn add_mark_sheets(
    wb: &mut Workbook,
    roster: &[Student],
    subjects: &[(String, &SubjectData)],
    scale: &Scale,
    used: &mut Vec<String>,
    locked: bool,
) -> AppResult<()> {
    let head = Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xB1D2E1))
        .set_border_bottom(FormatBorder::Medium)
        .set_text_wrap()
        .set_align(FormatAlign::Center)
        .set_align(FormatAlign::VerticalCenter);
    let pct = Format::new().set_num_format("0.0").set_bold();
    let num = Format::new().set_align(FormatAlign::Center);
    let code = Format::new().set_align(FormatAlign::Center).set_font_color(Color::RGB(0xB22222));
    for (name, sd) in subjects {
        let ws = wb.add_worksheet();
        ws.set_name(sheet_name(name, used)).map_err(err)?;
        if locked {
            ws.protect();
        }
        let fixed = ["Student #", "Last Name", "First Name", "Overall (calculated)", "Letter"];
        for (i, h) in fixed.iter().enumerate() {
            ws.write_with_format(0, i as u16, *h, &head).map_err(err)?;
        }
        for (j, a) in sd.assessments.iter().enumerate() {
            ws.write_with_format(0, (fixed.len() + j) as u16, header_for(a), &head).map_err(err)?;
            ws.set_column_width((fixed.len() + j) as u16, 14).map_err(err)?;
        }
        ws.set_row_height(0, 45).map_err(err)?;
        ws.set_column_width(1, 16).map_err(err)?;
        ws.set_column_width(2, 14).map_err(err)?;
        ws.set_column_width(3, 11).map_err(err)?;
        ws.set_freeze_panes(1, 3).map_err(err)?;

        for (i, s) in roster.iter().enumerate() {
            let row = (i + 1) as u32;
            ws.write(row, 0, &s.student_number).map_err(err)?;
            ws.write(row, 1, &s.last_name).map_err(err)?;
            ws.write(row, 2, &s.first_name).map_err(err)?;
            let r = sd.result_for(s.id);
            if let Some(p) = r.overall {
                ws.write_number_with_format(row, 3, (p * 10.0).round() / 10.0, &pct).map_err(err)?;
                ws.write(row, 4, scale.label(p)).map_err(err)?;
            }
            for (j, a) in sd.assessments.iter().enumerate() {
                let col = (fixed.len() + j) as u16;
                if let Some(m) = sd.mark(s.id, a.id) {
                    match (m.value, m.status) {
                        (Some(v), Status::Marked) if a.kind != "level" => {
                            ws.write_number_with_format(row, col, v, &num).map_err(err)?;
                        }
                        _ => {
                            ws.write_with_format(row, col, display_mark(Kind::parse(&a.kind), m), &code).map_err(err)?;
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
