use super::*;
use crate::code_tour::{self, Job};
use serde_json::Value;
use tanki_core::{
    code_tour::{Attachment, CodeTourRef},
    config::CodeTourSettings,
};

pub(super) struct TourActivity {
    pub card_id: i64,
    pub reference: CodeTourRef,
    pub job: Option<Job>,
    pub editors: Vec<Value>,
    pub opening: bool,
    pub status: String,
}

impl App<'_> {
    pub(crate) fn tour_holding(&self) -> bool {
        matches!(&self.screen, Screen::Review(v) if v.reviewer.holding())
    }
    pub(crate) fn tour_status(&self) -> Option<&str> {
        self.tour.as_ref().map(|t| t.status.as_str())
    }
    fn tour_settings(&self) -> Result<CodeTourSettings, String> {
        let settings = self
            .config_path
            .as_ref()
            .map(|p| tanki_core::config::Config::load(p))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or_default()
            .code_tour;
        if !settings.enabled {
            return Err("Enable [code_tour] in tanki config; see docs/neovim.md".into());
        }
        Ok(settings)
    }
    pub(super) fn open_code_tour(&mut self) {
        if self.tour_holding() {
            return;
        }
        let reference = match &self.screen {
            Screen::Review(v) if v.reviewer.phase() == Phase::Answer => {
                match v.reviewer.card().and_then(|c| c.queued.code_tour.clone()) {
                    Some(Attachment::Saved(r)) => r,
                    Some(Attachment::Invalid(e)) => {
                        self.error(e);
                        return;
                    }
                    None => {
                        self.notice("This note has no CodeTour reference");
                        return;
                    }
                }
            }
            _ => {
                self.notice("Reveal the answer before opening its code-tour");
                return;
            }
        };
        let settings = match self.tour_settings() {
            Ok(s) => s,
            Err(e) => {
                self.error(e);
                return;
            }
        };
        let now = self.now;
        let Some(view) = self.review() else {
            return;
        };
        let card_id = view.reviewer.card().unwrap().queued.id;
        if !view.reviewer.begin_tour(now) {
            return;
        }
        self.message = None;
        self.services.audio.stop();
        self.clear_images();
        let job = (self.tour_launcher)(settings, reference.clone(), None);
        self.tour = Some(TourActivity {
            card_id,
            reference,
            job: Some(job),
            editors: vec![],
            opening: false,
            status: "Finding Neovim editors…".into(),
        });
    }
    pub(super) fn choose_tour_editor(&mut self, index: usize) {
        let settings = match self.tour_settings() {
            Ok(s) => s,
            Err(e) => {
                self.resume_code_tour();
                self.error(e);
                return;
            }
        };
        let Some(tour) = self.tour.as_mut() else {
            return;
        };
        let Some(editor) = tour.editors.get(index).cloned() else {
            return;
        };
        self.tour_editor = Some(editor.clone());
        tour.status = "Opening saved tour…".into();
        tour.opening = true;
        tour.job = Some((self.tour_launcher)(
            settings,
            tour.reference.clone(),
            Some(editor),
        ));
    }
    pub(super) fn resume_code_tour(&mut self) {
        if self.tour.is_none() && !self.tour_holding() {
            return;
        }
        self.tour = None; // Cancels pending worker; its late response has no receiver.
        self.overlay = None;
        self.with_reviewer(|r, b, now| r.end_tour(b, now));
        self.after_editor();
        self.ignore_keys_before = Some(self.now.at);
    }
    pub(super) fn poll_code_tour(&mut self) {
        let Some(tour) = &self.tour else {
            return;
        };
        let still_current = matches!(&self.screen, Screen::Review(v) if v.reviewer.card().is_some_and(|c| c.queued.id == tour.card_id));
        if !still_current {
            self.resume_code_tour();
            return;
        }
        let Some(result) = tour.job.as_ref().and_then(Job::poll) else {
            return;
        };
        self.tour.as_mut().unwrap().job = None;
        match result {
            Err(error) => {
                self.resume_code_tour();
                self.tour_editor = None;
                self.error(format!("Could not open tour: {error}"));
            }
            Ok(result) if !self.tour.as_ref().unwrap().opening => {
                let Some(editors) = result.as_array() else {
                    self.resume_code_tour();
                    self.error("Invalid Neovim editor list");
                    return;
                };
                if editors.is_empty() {
                    let settings = match self.tour_settings() {
                        Ok(s) => s,
                        Err(e) => {
                            self.resume_code_tour();
                            self.error(e);
                            return;
                        }
                    };
                    let tour = self.tour.as_mut().unwrap();
                    tour.opening = true;
                    tour.status = "Starting Neovim in Herdr…".into();
                    tour.job = Some((self.tour_launcher)(
                        settings,
                        tour.reference.clone(),
                        Some(serde_json::json!({"launch":true})),
                    ));
                    return;
                }
                self.tour.as_mut().unwrap().editors = editors.clone();
                let remembered = self.tour_editor.as_ref().and_then(|old| {
                    editors.iter().position(|e| {
                        e["address"] == old["address"] && e["instance_id"] == old["instance_id"]
                    })
                });
                if let Some(index) = remembered {
                    self.choose_tour_editor(index);
                } else if editors.len() == 1 {
                    self.choose_tour_editor(0);
                } else {
                    self.tour.as_mut().unwrap().status = "Choose a Neovim editor".into();
                    self.overlay = Some(Overlay::Choice {
                        title: "Neovim editor".into(),
                        message: "Choose where to open the saved tour".into(),
                        options: editors
                            .iter()
                            .enumerate()
                            .map(|(i, e)| {
                                (
                                    format!(
                                        "{} · {}",
                                        e["cwd"].as_str().unwrap_or(""),
                                        e["address"].as_str().unwrap_or("")
                                    ),
                                    ChoiceValue::TourEditor(i),
                                )
                            })
                            .collect(),
                        selected: 0,
                        cancel: ChoiceValue::TourCancel,
                    });
                }
            }
            Ok(result) => {
                if result["editor"]["instance_id"].is_string() {
                    self.tour_editor = Some(result["editor"].clone());
                }
                let title = result["title"].as_str().unwrap_or("Saved tour");
                let issues = result["issues"].as_array().map_or(0, Vec::len);
                let counts = result
                    .get("counts")
                    .map(Value::to_string)
                    .unwrap_or_default();
                let focus = result["focus_error"]
                    .as_str()
                    .map(|e| format!(" · focus: {e}"))
                    .unwrap_or_default();
                self.tour.as_mut().unwrap().status =
                    format!("{title} · {issues} source issues · {counts}{focus}");
            }
        }
    }
}

pub(super) type TourLauncher = Box<dyn FnMut(CodeTourSettings, CodeTourRef, Option<Value>) -> Job>;
pub(super) fn launcher() -> TourLauncher {
    Box::new(|settings, reference, editor| {
        Job::spawn(move |cancel| {
            if let Some(editor) = editor {
                code_tour::open(&settings, &reference, &editor, cancel)
            } else {
                code_tour::editors(&settings, cancel)
            }
        })
    })
}

pub(super) struct TourPicker {
    job: Option<Job>,
    entries: Vec<Value>,
    limit: usize,
    context: (i64, Option<i64>),
}
impl App<'_> {
    pub(super) fn pick_code_tour(&mut self) {
        let Some(Layer::Add(ui)) = self.layers.last() else {
            return;
        };
        let Some(context) = ui.code_tour_context() else {
            self.error("This note type needs a CodeTour field");
            return;
        };
        self.tour_picker = Some(TourPicker {
            job: None,
            entries: vec![],
            limit: 50,
            context,
        });
        self.load_tour_picker();
    }
    pub(super) fn load_tour_picker(&mut self) {
        let settings = match self.tour_settings() {
            Ok(s) => s,
            Err(e) => {
                self.tour_picker = None;
                self.error(e);
                return;
            }
        };
        let Some(picker) = self.tour_picker.as_mut() else {
            return;
        };
        let limit = picker.limit;
        picker.job = Some(Job::spawn(move |cancel| {
            code_tour::list(&settings, limit, cancel)
        }));
        self.overlay = Some(Overlay::Choice {
            title: "Saved code-tours".into(),
            message: "Loading local saved metadata…".into(),
            options: vec![("Cancel".into(), ChoiceValue::TourPickerCancel)],
            selected: 0,
            cancel: ChoiceValue::TourPickerCancel,
        });
    }
    pub(super) fn poll_tour_picker(&mut self) {
        let Some(result) = self
            .tour_picker
            .as_ref()
            .and_then(|p| p.job.as_ref())
            .and_then(Job::poll)
        else {
            return;
        };
        self.tour_picker.as_mut().unwrap().job = None;
        match result {
            Err(e) => {
                self.tour_picker = None;
                self.overlay = None;
                self.error(e);
            }
            Ok(value) => {
                let Some(entries) = value["tours"].as_array() else {
                    self.tour_picker = None;
                    self.overlay = None;
                    self.error("Invalid tour metadata list");
                    return;
                };
                let picker = self.tour_picker.as_mut().unwrap();
                picker.entries = entries.clone();
                let mut options: Vec<_> = entries
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let labels = t["repositories"]
                            .as_array()
                            .map(|repos| {
                                repos
                                    .iter()
                                    .filter_map(|r| r["label"].as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            })
                            .unwrap_or_default();
                        (
                            format!(
                                "{} · {} · {} · {}",
                                t["title"].as_str().unwrap_or("Untitled"),
                                t["id"].as_str().unwrap_or("?"),
                                t["saved_at"].as_str().unwrap_or(""),
                                labels
                            ),
                            ChoiceValue::TourAttach(i),
                        )
                    })
                    .collect();
                if entries.len() == picker.limit && picker.limit < 10000 {
                    options.push(("Load more saved tours".into(), ChoiceValue::TourMore));
                }
                options.push(("Cancel".into(), ChoiceValue::TourPickerCancel));
                self.overlay = Some(Overlay::Choice {
                    title: "Saved code-tours".into(),
                    message: format!(
                        "Choose an existing tour. Library diagnostics: {}",
                        value["issues"]
                    ),
                    options,
                    selected: 0,
                    cancel: ChoiceValue::TourPickerCancel,
                });
            }
        }
    }
    pub(super) fn attach_picked_tour(&mut self, index: usize) {
        let Some(picker) = self.tour_picker.take() else {
            return;
        };
        let Some(entry) = picker.entries.get(index) else {
            return;
        };
        let r = match entry["id"]
            .as_str()
            .ok_or("Missing tour ID")
            .and_then(|s| CodeTourRef::from_id(s).map_err(|_| "Invalid tour ID"))
        {
            Ok(r) => r,
            Err(e) => {
                self.error(e);
                return;
            }
        };
        if let Some(Layer::Add(ui)) = self.layers.last_mut() {
            if ui.code_tour_context() == Some(picker.context) {
                ui.set_code_tour(&self.backend, r);
                self.notice("Code-tour attached to draft; save the note to keep it");
                return;
            }
        }
        self.error("Note form changed while selecting a tour; select again");
    }
    pub(super) fn more_tours(&mut self) {
        if let Some(picker) = self.tour_picker.as_mut() {
            picker.limit = (picker.limit * 2).min(10000);
        }
        self.load_tour_picker();
    }
}
