;;; piano-mode.el --- Play piano in Emacs with real key-up  -*- lexical-binding: t; -*-

;; Author: shyweeds
;; Version: 0.3.0
;; Package-Requires: ((emacs "28.1"))
;; Keywords: games, multimedia
;; URL: https://github.com/tinyblinker/emacs-piano-mode

;;; Commentary:

;; Turn your computer keyboard into a piano with real key-down/key-up
;; tracking.
;;
;; The companion `piano-keys' program (Rust, zero dependencies) reads
;; raw key state cross-platform -- X11 or evdev on Linux, user32 on
;; Windows, CoreGraphics on macOS -- without root or `input' group
;; membership, and reports "down KEY" / "up KEY" lines to Emacs.
;; Emacs maps them to MIDI notes and drives a persistent FluidSynth
;; process.
;;
;; `M-x piano-play' opens the piano buffer and starts everything.

;;; Code:

(require 'seq)
(require 'subr-x)

(defgroup piano nil
  "Play a piano from your computer keyboard."
  :group 'games
  :prefix "piano-")

(defcustom piano-keys
  '(("a" . 60) ("w" . 61) ("s" . 62) ("e" . 63) ("d" . 64)
    ("f" . 65) ("t" . 66) ("g" . 67) ("y" . 68) ("h" . 69)
    ("u" . 70) ("j" . 71) ("k" . 72) ("o" . 73) ("l" . 74)
    ("p" . 75) (";" . 76))
  "Alist mapping key names to MIDI note numbers.

Key names are single letters, digits or punctuation accepted by the
`piano-keys' program; letters and digits are portable across
platforms."
  :type '(alist :key-type string :value-type integer)
  :set (lambda (symbol value)
         (set-default symbol value)
         (when (boundp 'piano-mode-map)
           (piano--rebuild-map))))

(defcustom piano-keys-program "piano-keys"
  "Name (or path) of the native key reader program."
  :type 'string)

(defcustom piano-keys-extra-args nil
  "Extra arguments passed to `piano-keys-program'.
For example (\"--backend\" \"x11\") forces the X11 backend on Linux."
  :type '(repeat string))

(defcustom piano-poll-ms 4
  "Keyboard polling interval in milliseconds."
  :type 'integer)

(defcustom piano-input 'auto
  "Keyboard input source.
`auto' uses `piano-keys-program' when it is available and falls
back to Emacs key events (timer-based note release) otherwise."
  :type '(choice (const auto) (const keys-helper) (const emacs-keys)))

(defcustom piano-require-piano-buffer t
  "When non-nil, ignore key-reader events unless the piano buffer is selected.
This keeps letters typing normally in other buffers while the global
key reader is running."
  :type 'boolean)

(defcustom piano-note-duration 0.5
  "Note length in seconds for the `emacs-keys' input fallback."
  :type 'number)

(defcustom piano-transpose 0
  "Semitones added to every note."
  :type 'integer)

(defcustom piano-velocity 100
  "MIDI velocity (1-127) sent with every note-on."
  :type 'integer)

(defcustom piano-synth-program "fluidsynth"
  "Name (or path) of the FluidSynth executable."
  :type 'string)

(defcustom piano-soundfont nil
  "SoundFont file for FluidSynth.
If nil, try `PIANO_SOUNDFONT' and common system locations."
  :type '(choice (const :tag "Auto-detect" nil) file))

(defcustom piano-audio-driver nil
  "FluidSynth audio driver, e.g. \"alsa\", \"pipewire\", \"coreaudio\".
Nil lets FluidSynth pick its default."
  :type '(choice (const :tag "FluidSynth default" nil) string))

(defcustom piano-gain 0.5
  "FluidSynth master gain."
  :type 'number)

;; Note state is a global resource (sound comes from one global
;; process), so these variables are not buffer-local.
(defvar piano--active (make-hash-table :test #'eql)
  "MIDI notes currently sounding.")
(defvar piano--timers (make-hash-table :test #'eql)
  "Note-off timers for the `emacs-keys' fallback input.")
(defvar piano--keys-process nil
  "The native key reader process.")
(defvar piano--synth-process nil
  "The FluidSynth process.")
(defvar piano--stopping nil
  "Non-nil while `piano--stop' is running.")
(defvar piano--piano-buffer nil
  "The dedicated piano buffer created by `piano-play'.")
(defvar piano--focused-p nil
  "Whether the piano buffer currently has focus.")

(defun piano--soundfont ()
  "Return a readable SoundFont file name."
  (or piano-soundfont
      (getenv "PIANO_SOUNDFONT")
      (seq-find #'file-readable-p
                '("/usr/share/sounds/sf2/FluidR3_GM.sf2"
                  "/usr/share/soundfonts/FluidR3_GM.sf2"
                  "/usr/share/sounds/sf2/default-GM.sf2"))))

(defun piano--synth-send (format-string &rest args)
  "Send the shell command FORMAT-STRING (formatted with ARGS) to FluidSynth."
  (when (process-live-p piano--synth-process)
    (process-send-string piano--synth-process
                         (apply #'format format-string args))))

(defun piano--note-on (midi)
  "Start MIDI note MIDI unless it is already sounding."
  (unless (gethash midi piano--active)
    (puthash midi t piano--active)
    (piano--synth-send "noteon 0 %d %d\n" midi piano-velocity)))

(defun piano--note-off (midi)
  "Stop MIDI note MIDI."
  (when (gethash midi piano--active)
    (remhash midi piano--active)
    (piano--synth-send "noteoff 0 %d\n" midi)))

(defun piano--all-notes-off ()
  "Release every sounding note."
  (maphash (lambda (midi _active)
             (piano--synth-send "noteoff 0 %d\n" midi))
           piano--active)
  (clrhash piano--active))

(defun piano--emacs-key-note ()
  "Play the note bound to the key that invoked this command.

Used by the `emacs-keys' fallback input; releases the note after
`piano-note-duration' seconds."
  (let* ((key (key-description (vector last-command-event)))
         (midi (cdr (assoc key piano-keys))))
    (unless midi
      (user-error "No piano note bound to %s" key))
    (let ((note (+ midi piano-transpose)))
      (piano--note-off note)
      (piano--note-on note)
      (let ((timer (gethash note piano--timers)))
        (when timer
          (cancel-timer timer)))
      (puthash note
               (run-at-time piano-note-duration nil #'piano--note-off note)
               piano--timers))))

(defun piano-play-key ()
  "Handle a piano key press from the Emacs side.

With the native key reader active this is a no-op, because the reader
reports the real key-down and key-up events; otherwise the note plays
with a timer-based release."
  (interactive)
  (unless (process-live-p piano--keys-process)
    (piano--emacs-key-note)))

(defvar piano-mode-map nil
  "Keymap for `piano-mode'.

The bindings shadow the keys read by the native helper so they do not
self-insert while the helper listens globally.")

(defun piano--rebuild-map ()
  "Rebuild `piano-mode-map' from `piano-keys'."
  (let ((map (make-sparse-keymap)))
    (dolist (binding piano-keys)
      (define-key map (car binding) #'piano-play-key))
    ;; The bound keys are read globally by piano-keys; make sure they
    ;; do not self-insert in piano buffers.
    (define-key map [remap self-insert-command] #'ignore)
    (setq piano-mode-map map)))

(piano--rebuild-map)

(defun piano--helper-keys-argument ()
  "Return the --keys argument derived from `piano-keys'."
  (mapconcat (lambda (binding) (upcase (car binding))) piano-keys ","))

(defun piano--helper-active-p ()
  "Return non-nil when key-reader events should be processed."
  (or (not piano-require-piano-buffer)
      (null piano--piano-buffer)
      (eq (window-buffer (selected-window)) piano--piano-buffer)))

(defun piano--apply-key-event (action key)
  "Turn an ACTION (\"down\"/\"up\") event for KEY into note-on/note-off."
  (let ((midi (cdr (assoc (downcase key) piano-keys))))
    (when midi
      (pcase action
        ("down" (piano--note-on (+ midi piano-transpose)))
        ("up" (piano--note-off (+ midi piano-transpose)))))))

(defun piano--handle-keys-line (line)
  "Dispatch one LINE of key-reader output."
  (pcase (split-string line " " t)
    (`(,action ,key)
     (when (piano--helper-active-p)
       (piano--apply-key-event action key)))
    (_ (unless (string-empty-p line)
         (message "piano-keys: unrecognized line %S" line)))))

(defun piano--keys-filter (process string)
  "Parse \"down KEY\" / \"up KEY\" lines from the key reader PROCESS.
STRING is the latest chunk of process output; incomplete lines are
kept in a process property until they are finished."
  (let ((pending (concat (process-get process 'piano-pending) string)))
    (while (string-match "\n" pending)
      ;; Grab both substrings before dispatching: the handler calls
      ;; regex functions that would clobber the match data.
      (let ((line (substring pending 0 (match-beginning 0))))
        (setq pending (substring pending (match-end 0)))
        (piano--handle-keys-line line)))
    (process-put process 'piano-pending pending)))

(defun piano--keys-sentinel (process event)
  "Handle the key reader PROCESS exiting with EVENT."
  (when (memq (process-status process) '(exit signal))
    (when (eq process piano--keys-process)
      (setq piano--keys-process nil))
    (unless piano--stopping
      (let ((error-output (with-current-buffer " *piano-keys-errors*"
                            (buffer-string))))
        (message "piano-keys exited%s%s"
                 (if (string-empty-p error-output)
                     ""
                   (format " (%s)" (string-trim error-output)))
                 (if (string-empty-p (string-trim event))
                     ""
                   (format ": %s" (string-trim event))))))))

(defun piano--start-helper ()
  "Start the native key reader."
  (unless (process-live-p piano--keys-process)
    (let ((program (or (executable-find piano-keys-program)
                       (user-error "Program %S not found" piano-keys-program))))
      (with-current-buffer (get-buffer-create " *piano-keys-errors*")
        (erase-buffer))
      (setq piano--keys-process
            (make-process
             :name "piano-keys"
             :command (append (list program
                                    "--keys" (piano--helper-keys-argument)
                                    "--poll-ms" (number-to-string piano-poll-ms))
                              piano-keys-extra-args)
             :connection-type 'pipe
             :coding 'utf-8-unix
             :noquery t
             :buffer (generate-new-buffer " *piano-keys*")
             :stderr (get-buffer-create " *piano-keys-errors*")
             :filter #'piano--keys-filter
             :sentinel #'piano--keys-sentinel)))))

(defun piano--window-changed (window)
  "Release all notes when WINDOW's buffer stops being the piano buffer."
  (let ((focused (and piano--piano-buffer
                      (eq (window-buffer window) piano--piano-buffer))))
    (when (and piano--focused-p (not focused))
      (piano--all-notes-off))
    (setq piano--focused-p focused)))

(defun piano--synth-sentinel (process event)
  "Handle the FluidSynth PROCESS exiting with EVENT."
  (when (memq (process-status process) '(exit signal))
    (when (eq process piano--synth-process)
      (setq piano--synth-process nil))
    (unless piano--stopping
      (message "FluidSynth exited: %s" (string-trim event)))))

(defun piano--start-synth ()
  "Start FluidSynth unless it is already running."
  (unless (process-live-p piano--synth-process)
    (let ((program (or (executable-find piano-synth-program)
                       (user-error "Program %S not found" piano-synth-program)))
          (soundfont (or (piano--soundfont)
                         (user-error "No SoundFont; customize `piano-soundfont'"))))
      (setq piano--synth-process
            (make-process
             :name "piano-fluidsynth"
             :command (append (list program
                                    "-n" "-g" (number-to-string piano-gain))
                              (when piano-audio-driver
                                (list "-a" piano-audio-driver))
                              (list soundfont))
             :connection-type 'pipe
             :coding 'utf-8-unix
             :noquery t
             :buffer (generate-new-buffer " *piano-fluidsynth*")
             :sentinel #'piano--synth-sentinel)))))

(defun piano--start ()
  "Start the synthesizer and the key reader."
  (piano--start-synth)
  (pcase piano-input
    ('keys-helper (piano--start-helper))
    ('emacs-keys nil)
    ('auto (if (executable-find piano-keys-program)
               (piano--start-helper)
             (message "piano-keys not found; using Emacs keys (timer-based release)"))))
  (add-hook 'window-buffer-change-functions #'piano--window-changed))

(defun piano--stop ()
  "Stop the key reader, release all notes, stop the synthesizer."
  (let ((piano--stopping t))
    (remove-hook 'window-buffer-change-functions #'piano--window-changed)
    (when (process-live-p piano--keys-process)
      (delete-process piano--keys-process))
    (setq piano--keys-process nil
          piano--focused-p nil
          piano--piano-buffer nil)
    (maphash (lambda (_midi timer) (cancel-timer timer)) piano--timers)
    (clrhash piano--timers)
    (piano--all-notes-off)
    (when (process-live-p piano--synth-process)
      (ignore-errors
        (piano--synth-send "cc 0 123 0\nquit\n"))
      (let ((process piano--synth-process))
        (accept-process-output process 0.2)
        (when (process-live-p process)
          (delete-process process))))
    (setq piano--synth-process nil)))

(defun piano--kill-buffer ()
  "Stop piano mode when its buffer is killed."
  (when (and (boundp 'piano-mode) piano-mode)
    (piano-mode -1)))

;;;###autoload
(define-minor-mode piano-mode
  "Play piano notes from the computer keyboard.
\\{piano-mode-map}"
  :lighter " Piano"
  :keymap piano-mode-map
  (if piano-mode
      (condition-case error
          (piano--start)
        (error
         (setq piano-mode nil)
         (signal (car error) (cdr error))))
    (piano--stop)))

;;;###autoload
(defun piano-play ()
  "Open a dedicated piano buffer and start playing."
  (interactive)
  (let ((buffer (get-buffer-create "*piano*")))
    (with-current-buffer buffer
      (unless (derived-mode-p 'special-mode)
        (special-mode))
      (piano--show-keys))
    (pop-to-buffer buffer)
    (setq piano--piano-buffer buffer)
    (add-hook 'kill-buffer-hook #'piano--kill-buffer nil t)
    (piano-mode 1)))

(defun piano--show-keys ()
  "Insert the current key layout into the current buffer."
  (let ((inhibit-read-only t))
    (erase-buffer)
    (insert "piano-mode — the keys below are read globally by piano-keys\n"
            "white keys: "
            (mapconcat (lambda (binding) (upcase (car binding)))
                       piano-keys " ")
            "\n"
            "M-x piano-mode stops the instrument; kill this buffer to quit\n")
    (add-text-properties (point-min) (point-max)
                         '(piano-layout t))))

(add-hook 'kill-emacs-hook #'piano--stop)

(provide 'piano-mode)
;;; piano-mode.el ends here
