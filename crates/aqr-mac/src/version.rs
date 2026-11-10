//! The versions of an `.aqr` archive (the points of its journal, `docs/append-design.md`,
//! "Journaling"), as Finder's services have them:
//!
//! - "Name this version with Aqueous…": an alert asks for a name (`{today}` at first), and the
//!   state the archive is in is named by it (`aqr --tag`, which fills in `{today}` and `{now}`).
//! - "Extract a previous version with Aqueous…": an alert lists the versions named
//!   (`aqr --history --json`), the last chosen, and the one chosen is extracted as it was
//!   (`aqr -d --at`) into a folder beside the archive named after it
//!   ([`aqr_ipc::version_folder`]), a job as the others are ([`crate::job::Job::start_at`]).
//!
//! As Windows' (`crates/aqr-win/src/version.rs`), of one archive at a time.

use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSActionCell, NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSBorderType, NSCell, NSLineBreakMode, NSScrollView,
    NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewStyle, NSTextAlignment, NSTextField, NSTextFieldCell,
};
use objc2_foundation::{NSIndexSet, NSInteger, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use aqr_ipc::{Point, file_name};
use aqr_l10n::{common, mac, services};

use crate::app::{activate, alert};

/// The archive of a service of one: the only path given, else none (and the user told so).
pub fn one(mtm: MainThreadMarker, mut paths: Vec<PathBuf>, service: &str) -> Option<PathBuf> {
    if paths.len() > 1 {
        alert(mtm, NSAlertStyle::Warning, mac::one_archive(), &mac::one_archive_info(service), None);
        return None;
    }
    paths.pop()
}

/// "Name this version": asks for a name for the state `archive` is in, and names it.
pub fn name(mtm: MainThreadMarker, archive: &Path) {
    let shown = file_name(archive);
    let Some(label) = ask_name(mtm, &shown) else { return };
    match aqr_ipc::tag(&aqr_ipc::aqr(), archive, &label) {
        Ok(told) => alert(mtm, NSAlertStyle::Informational, &mac::version_named(&shown), &told, None),
        Err(e) => alert(mtm, NSAlertStyle::Critical, &mac::could_not_name(&shown), &e, None),
    }
}

/// "Extract a previous version": the version of `archive` to extract, as the user chooses it;
/// none where there are none (the user told how they are made) or the user cancels.
pub fn choose(mtm: MainThreadMarker, archive: &Path) -> Option<Point> {
    let shown = file_name(archive);
    let mut points = match aqr_ipc::points(&aqr_ipc::aqr(), archive) {
        Ok(p) => p,
        Err(e) => {
            alert(mtm, NSAlertStyle::Critical, &mac::could_not_list(&shown), &e, None);
            return None;
        }
    };
    if points.is_empty() {
        let info = mac::no_versions_info(services::name_version());
        alert(mtm, NSAlertStyle::Informational, &mac::no_versions(&shown), &info, None);
        return None;
    }
    let k = ask_point(mtm, &shown, &points)?;
    Some(points.swap_remove(k))
}

/// Asks for the name of a version of archive `archive` (as it is shown): a field of `{today}`,
/// to be changed; `None` where the user cancels.
fn ask_name(mtm: MainThreadMarker, archive: &str) -> Option<String> {
    let a = NSAlert::new(mtm);
    a.setAlertStyle(NSAlertStyle::Informational);
    a.setMessageText(&NSString::from_str(&mac::name_version(archive)));
    a.setInformativeText(&NSString::from_str(mac::name_version_info()));
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0));
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
    field.setStringValue(&NSString::from_str("{today}"));
    field.setPlaceholderString(Some(&NSString::from_str(mac::version_name())));
    a.setAccessoryView(Some(&field));
    a.addButtonWithTitle(&NSString::from_str(common::ok()));
    a.addButtonWithTitle(&NSString::from_str(common::cancel()));
    a.layout();
    // (the field first, its text all selected)
    a.window().setInitialFirstResponder(Some(&field));
    activate(mtm);
    (a.runModal() == NSAlertFirstButtonReturn).then(|| field.stringValue().to_string())
}

/// The rows of the list of versions, by column: the number, when it was named, its name.
pub struct Columns(Vec<[Retained<NSString>; 3]>);

impl Columns {
    /// What is shown in `column` (its identifier the index) of row `row`.
    fn value(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<AnyObject>> {
        let k: usize = column?.identifier().to_string().parse().ok()?;
        let s = self.0.get(usize::try_from(row).ok()?)?.get(k)?.clone();
        Some(Retained::into_super(Retained::into_super(s)))
    }
}

define_class!(
    // SAFETY: NSObject has no requirements of a subclass; `Rows` does not implement `Drop`.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Columns]
    struct Rows;

    // SAFETY: no requirements
    unsafe impl NSObjectProtocol for Rows {}

    // SAFETY: no requirements; the signatures are the protocol's
    unsafe impl NSTableViewDataSource for Rows {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _: &NSTableView) -> NSInteger {
            self.ivars().0.len() as NSInteger
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn value(
            &self,
            _: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<AnyObject>> {
            self.ivars().value(column, row)
        }
    }
);

define_class!(
    // SAFETY: NSTextFieldCell has no requirements of a subclass; `Centered` does not implement
    // `Drop`, and has no ivars (copied as cells are).
    #[unsafe(super(NSTextFieldCell, NSActionCell, NSCell, NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Centered;

    impl Centered {
        /// The text centred in the row (a cell's is at its top, the row taller than the text).
        #[unsafe(method(drawingRectForBounds:))]
        fn drawing_rect(&self, bounds: NSRect) -> NSRect {
            // SAFETY: NSCell's method of the same signature
            let mut r: NSRect = unsafe { msg_send![super(self), drawingRectForBounds: bounds] };
            let h = self.cellSizeForBounds(r).height;
            if h < r.size.height {
                r.origin.y += (r.size.height - h) / 2.0;
                r.size.height = h;
            }
            r
        }
    }
);

impl Centered {
    fn new(mtm: MainThreadMarker, alignment: NSTextAlignment) -> Retained<Self> {
        // SAFETY: NSTextFieldCell's `initTextCell:`
        let this: Retained<Self> = unsafe { msg_send![Self::alloc(mtm), initTextCell: &*NSString::from_str("")] };
        this.setAlignment(alignment);
        this.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        this
    }
}

impl Rows {
    fn new(mtm: MainThreadMarker, points: &[Point]) -> Retained<Self> {
        let rows = points
            .iter()
            .map(|p| [format!("#{}", p.number), p.shown.clone(), p.label.clone()].map(|s| NSString::from_str(&s)));
        let this = Self::alloc(mtm).set_ivars(Columns(rows.collect()));
        // SAFETY: NSObject's `init`
        unsafe { msg_send![super(this), init] }
    }
}

/// The widths of the list's columns (the number, when, the name), in points.
const WIDTHS: [f64; 3] = [48.0, 150.0, 240.0];

/// Asks which of `points` (versions of archive `archive`, as it is shown), the last chosen at
/// first: its index; `None` where the user cancels. A double-click chooses as OK does.
fn ask_point(mtm: MainThreadMarker, archive: &str, points: &[Point]) -> Option<usize> {
    let a = NSAlert::new(mtm);
    a.setAlertStyle(NSAlertStyle::Informational);
    a.setMessageText(&NSString::from_str(&mac::choose_version(archive)));
    a.setInformativeText(&NSString::from_str(mac::choose_version_info()));
    let width = WIDTHS.iter().sum::<f64>() + 24.0;
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 240.0));
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(NSBorderType::BezelBorder);
    let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), frame);
    for (k, w) in WIDTHS.iter().enumerate() {
        let c = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &NSString::from_str(&k.to_string()));
        c.setWidth(*w);
        c.setEditable(false);
        // (the numbers to the right, clear of the table's edge)
        let align = if k == 0 { NSTextAlignment::Right } else { NSTextAlignment::Left };
        // SAFETY: a text cell, as the column's data cells are
        unsafe { c.setDataCell(&Centered::new(mtm, align)) };
        table.addTableColumn(&c);
    }
    table.setHeaderView(None);
    table.setStyle(NSTableViewStyle::Plain);
    table.setUsesAlternatingRowBackgroundColors(true);
    table.setAllowsEmptySelection(false);
    // (the table holds its data source weakly: `rows` outlives the alert)
    let rows = Rows::new(mtm, points);
    // SAFETY: a data source of the table's rows, living as long as it is shown
    unsafe { table.setDataSource(Some(ProtocolObject::from_ref(&*rows))) };
    scroll.setDocumentView(Some(&table));
    table.reloadData();
    let last = points.len() - 1;
    table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(last), false);
    table.scrollRowToVisible(last as NSInteger);
    a.setAccessoryView(Some(&scroll));
    let ok = a.addButtonWithTitle(&NSString::from_str(common::ok()));
    a.addButtonWithTitle(&NSString::from_str(common::cancel()));
    // SAFETY: the OK button, which outlives the table's use (both the alert's), clicked by its
    // `performClick:` of one object argument
    let target: &AnyObject = &ok;
    unsafe {
        table.setTarget(Some(target));
        table.setDoubleAction(Some(sel!(performClick:)));
    }
    a.layout();
    a.window().setInitialFirstResponder(Some(&table));
    activate(mtm);
    let chosen = a.runModal() == NSAlertFirstButtonReturn;
    let row = table.selectedRow();
    drop(rows);
    if chosen { usize::try_from(row).ok().filter(|&r| r < points.len()) } else { None }
}
