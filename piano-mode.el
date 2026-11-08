;;; piano-mode.el --- Play piano with real key-up  -*- lexical-binding: t; -*-

;; Author: shyweeds
;; Maintainer: shyweeds
;; Version: 0.3.1
;; Package-Requires: ((emacs "28.1"))
;; Keywords: games, multimedia
;; URL: https://github.com/tinyblinker/emacs-piano-mode

;;; Commentary:

;; Turn your computer keyboard into a piano with real key-down/key-up
;; tracking.
;;
;; The companion `piano-keys' program (Rust, zero dependencies) reads
;; raw key state from the kernel evdev ABI on Linux without root or
;; `input' group membership, and reports "down KEY" / "up KEY" lines
;; to Emacs.  Emacs maps them to MIDI notes and drives a persistent
;; FluidSynth process.
;;
;; `M-x piano-play' opens the piano buffer and starts everything.

;;; Code:

(require 'seq)
(require 'subr-x)

(defun piano--guess-load-dir ()
  "Return the directory piano-mode was loaded from, or nil.
When `load-file-name' names a native-compiled .eln inside the eln
cache, ask `locate-library' for the installed source directory
instead."
  (let ((file (or (and (boundp 'load-file-name) load-file-name)
                  (buffer-file-name))))
    (cond
     ((and file (not (string-suffix-p ".eln" file)))
      (file-name-directory file))
     ((when-let* ((library (locate-library "piano-mode")))
        (file-name-directory library)))
     (file
      (file-name-directory file)))))

(defvar piano--load-dir (piano--guess-load-dir)
  "Directory the package was loaded from, or nil.
Used to find the SoundFont and the native key reader bundled next
to the loaded file.")

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

Each element is (KEY . MIDI).  KEY is a single letter, digit or
punctuation character accepted by `piano-keys-program'; MIDI is a
note number, with middle C as 60.  Changing this rebuilds the keymap
and restarts a running key reader."
  :group 'piano
  :type '(alist :key-type string :value-type integer)
  :set (lambda (symbol value)
         (set-default symbol value)
         ;; piano-mode-map is unbound while the file is still loading,
         ;; so everything after the guard only runs on real changes.
         (when (boundp 'piano-mode-map)
           (piano--rebuild-map)
           (piano--restart-helper))))

(defcustom piano-keys-program "piano-keys"
  "Name (or path) of the native key reader program."
  :group 'piano
  :type 'string)

(defcustom piano-keys-extra-args nil
  "Extra arguments passed to `piano-keys-program'.
For example (\"--device\" \"/dev/input/event3\") pins one keyboard
device."
  :group 'piano
  :type '(repeat string))

(defcustom piano-auto-build t
  "Whether to build the bundled `piano-keys' reader when it is missing.
The package ships the reader's Rust source under piano_core/.  When
this is non-nil and a Rust toolchain is available, the first call to
`piano-play' builds the reader in place; set it to nil to require a
prebuilt `piano-keys' on the variable `exec-path' or in
`piano-keys-program'."
  :group 'piano
  :type 'boolean)

(defcustom piano-device nil
  "Evdev device path used by the key reader, or nil.

Nil selects the device on the first start: the key reader lists the
keyboards it can see and you pick one with completion; a single
keyboard is picked automatically.  A string pins the reader to that
device, e.g. \"/dev/input/by-id/usb-Keychron_K8_Pro-event-kbd\".
`piano-select-device' re-runs the interactive choice and saves it."
  :group 'piano
  :type '(choice (const :tag "Ask when needed" nil) string))

(defcustom piano-poll-ms 4
  "Keyboard polling interval in milliseconds."
  :group 'piano
  :type 'integer)

(defcustom piano-transpose 0
  "Semitones added to every note."
  :group 'piano
  :type 'integer)

(defcustom piano-velocity 100
  "MIDI velocity (1-127) sent with every note-on."
  :group 'piano
  :type 'integer)

(defcustom piano-synth-program "fluidsynth"
  "Name (or path) of the FluidSynth executable."
  :group 'piano
  :type 'string)

(defcustom piano-soundfont nil
  "SoundFont file for FluidSynth.
If nil, try `PIANO_SOUNDFONT', a TimGM6mb.sf2 next to the loaded
package, and common system locations."
  :group 'piano
  :type '(choice (const :tag "Auto-detect" nil) file))

(defcustom piano-audio-driver nil
  "FluidSynth audio driver, e.g. \"alsa\" or \"pipewire\".
Nil lets FluidSynth pick its default."
  :group 'piano
  :type '(choice (const :tag "FluidSynth default" nil) string))

(defcustom piano-gain 0.5
  "FluidSynth master gain."
  :group 'piano
  :type 'number)

;; Note state is a global resource (sound comes from one global
;; process), so these variables are not buffer-local.
(defvar piano--active (make-hash-table :test #'eql)
  "MIDI notes currently sounding.")
(defvar piano--keys-process nil
  "The native key reader process.")
(defvar piano--synth-process nil
  "The FluidSynth process.")
(defvar piano--stopping nil
  "Non-nil while `piano--stop' is running.")
(defvar piano--synth-starting nil
  "Non-nil while `piano--start-synth' waits for FluidSynth to start.")
(defvar piano--piano-buffer nil
  "The dedicated piano buffer created by `piano-play'.")
(defvar piano--focused-p nil
  "Whether the piano buffer currently has focus.")

(defun piano--soundfont ()
  "Return a readable SoundFont file name."
  (or piano-soundfont
      (getenv "PIANO_SOUNDFONT")
      (seq-find #'file-readable-p
                (delq nil
                      (list (when piano--load-dir
                              (expand-file-name
                               "piano_core/src/TimGM6mb.sf2" piano--load-dir))
                            "/usr/share/sounds/sf2/FluidR3_GM.sf2"
                            "/usr/share/soundfonts/FluidR3_GM.sf2"
                            "/usr/share/sounds/sf2/default-GM.sf2")))))

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

(defun piano-play-key ()
  "Shadow a mapped key so it does not self-insert in piano buffers.
The key reader reports the real key-down and key-up events."
  (interactive))

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

(defun piano--find-keys-program ()
  "Return the piano-keys executable, or nil.

Checks `piano-keys-program' on the search path, and also the build
outputs inside this repository, so loading the package straight from
a checkout just works."
  (or (executable-find piano-keys-program)
      (when piano--load-dir
        (seq-find #'file-executable-p
                  (list (expand-file-name "piano_core/target/release/piano-keys"
                                          piano--load-dir)
                        (expand-file-name "piano_core/target/debug/piano-keys"
                                          piano--load-dir))))))

(defun piano--keys-source-directory ()
  "Return the bundled piano_core source directory, or nil."
  (when piano--load-dir
    (let ((directory (expand-file-name "piano_core" piano--load-dir)))
      (when (file-exists-p (expand-file-name "Cargo.toml" directory))
        directory))))

(defun piano--find-cargo ()
  "Return the Cargo executable, or nil."
  (or (executable-find "cargo")
      (let ((cargo (expand-file-name "~/.cargo/bin/cargo")))
        (and (file-executable-p cargo) cargo))))

(defun piano--build-keys-program ()
  "Build the bundled piano-keys reader and return its path.
Return nil when auto-building is disabled, `piano-keys-program' is
customized, or no source tree or Rust toolchain is available.
Signal a `user-error' when the build fails."
  (when (and piano-auto-build
             (string= piano-keys-program "piano-keys"))
    (when-let* ((directory (piano--keys-source-directory))
                (cargo (piano--find-cargo)))
      (message "piano-mode: building piano-keys (first run)...")
      (let ((buffer (get-buffer-create " *piano-keys-build*")))
        (with-current-buffer buffer
          (erase-buffer))
        (let ((status (call-process
                       cargo nil buffer nil
                       "build" "--release" "--locked"
                       "--manifest-path"
                       (expand-file-name "Cargo.toml" directory))))
          (unless (and (integerp status) (zerop status))
            (user-error "Building piano-keys failed; see buffer %s"
                        (buffer-name buffer)))))
      (message "piano-mode: piano-keys built")
      (piano--find-keys-program))))

(defun piano--ensure-keys-program ()
  "Return the piano-keys executable, building it when necessary."
  (or (piano--find-keys-program)
      (piano--build-keys-program)
      (user-error "Key reader piano-keys not found (%S); install Rust and \
run \"cargo build --release\" in %s, or set `piano-keys-program'"
                  piano-keys-program
                  (or (piano--keys-source-directory) "piano_core/"))))

(defun piano--parse-devices (text)
  "Return an alist of (LABEL . PATH) for the keyboard lines in TEXT."
  (let ((devices nil))
    (dolist (line (split-string text "\n"))
      (let ((fields (split-string line "\t")))
        (when (equal (nth 2 fields) "yes")
          (let* ((path (nth 0 fields))
                 (name (nth 1 fields))
                 (label (if (string-empty-p (or name ""))
                            path
                          (format "%s (%s)" name path))))
            (push (cons label path) devices)))))
    (nreverse devices)))

(defun piano--list-devices (program)
  "Return an alist of (LABEL . PATH) for the keyboards PROGRAM reports."
  (with-temp-buffer
    ;; stdout and stderr both land here, so a failure's stderr becomes
    ;; part of the error message below.
    (unless (zerop (call-process program nil t nil "--list-devices"))
      (user-error
       "Listing keyboards failed (%s); check read access to /dev/input \
(udev uaccess or the input group)"
       (string-trim (buffer-string))))
    (piano--parse-devices (buffer-string))))

(defun piano--select-device (program)
  "Return the evdev device path to use, or nil for auto-detection.

Uses `piano-device' when set.  Otherwise asks PROGRAM for its
keyboards and lets the user pick one; the pick is saved to
`piano-device', so the prompt appears only once."
  (if (and (stringp piano-device) (not (string-empty-p piano-device)))
      piano-device
    (let ((devices (piano--list-devices program)))
      (cond ((null devices)
             (user-error
              "No readable keyboard found under /dev/input; add yourself \
to the input group (or grant udev uaccess access) and try again"))
            ((null (cdr devices))
             ;; Only one keyboard: use it without asking.
             (cdar devices))
            (t
             (let* ((choices (cons "Auto-detect (first match)"
                                   (mapcar #'car devices)))
                    (choice (completing-read "Keyboard device: " choices nil t)))
               (unless (string-prefix-p "Auto-detect" choice)
                 (let ((path (cdr (assoc choice devices))))
                   (customize-save-variable 'piano-device path)
                   (message "Piano keyboard device saved: %s" path)
                   path))))))))

;;;###autoload
(defun piano-select-device ()
  "Choose the keyboard device for `piano-mode' and save the choice."
  (interactive)
  (let* ((piano-device nil)              ; force a fresh prompt
         (program (piano--ensure-keys-program))
         (device (piano--select-device program)))
    (customize-save-variable 'piano-device device)
    ;; A running reader still reads the old device: restart it so the
    ;; new choice takes effect immediately.
    (piano--restart-helper)
    (message "Piano keyboard device: %s" (or device "auto-detect"))))

(defun piano--helper-active-p ()
  "Return non-nil when the piano buffer exists and has focus.

Events are only processed while the dedicated piano buffer is shown
in the selected window, so other buffers keep normal typing."
  (and piano--piano-buffer
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
      (piano--all-notes-off)
      (let ((tail (string-trim (with-current-buffer " *piano-keys-errors*"
                                 (buffer-string)))))
        (message "The piano-keys reader exited: %s"
                 (if (string-empty-p tail) (string-trim event) tail))))))

(defun piano--start-helper ()
  "Start the native key reader."
  (unless (process-live-p piano--keys-process)
    (let* ((program (piano--ensure-keys-program))
           (device (piano--select-device program)))
      (with-current-buffer (get-buffer-create " *piano-keys-errors*")
        (erase-buffer))
      (setq piano--keys-process
            (make-process
             :name "piano-keys"
             :command (append (list program
                                    "--keys" (piano--helper-keys-argument)
                                    "--poll-ms" (number-to-string piano-poll-ms))
                              (when device (list "--device" device))
                              piano-keys-extra-args)
             :connection-type 'pipe
             :coding 'utf-8-unix
             :noquery t
             :buffer (generate-new-buffer " *piano-keys*")
             :stderr (get-buffer-create " *piano-keys-errors*")
             :filter #'piano--keys-filter
             :sentinel #'piano--keys-sentinel)))))

(defun piano--restart-helper ()
  "Restart the key reader, if it is running, with the current device."
  (when (process-live-p piano--keys-process)
    (piano--all-notes-off)              ; keys held on the old device
    (let ((piano--stopping t))          ; keep the sentinel quiet
      (delete-process piano--keys-process))
    (setq piano--keys-process nil)
    (piano--start-helper)))

(defun piano--window-changed (window)
  "Release all notes when WINDOW stops showing the piano buffer."
  (let ((focused (and piano--piano-buffer
                      (eq (window-buffer window) piano--piano-buffer))))
    (when (and piano--focused-p (not focused))
      (piano--all-notes-off))
    (setq piano--focused-p focused)))

(defun piano--synth-error-tail ()
  "Return the tail of FluidSynth's stderr, or an empty string."
  (with-current-buffer (get-buffer-create " *piano-fluidsynth-errors*")
    (string-trim (buffer-string))))

(defun piano--synth-sentinel (process event)
  "Handle the FluidSynth PROCESS exiting with EVENT."
  (when (memq (process-status process) '(exit signal))
    (when (eq process piano--synth-process)
      (setq piano--synth-process nil))
    (unless (or piano--stopping piano--synth-starting)
      (message "FluidSynth exited: %s"
               (let ((tail (piano--synth-error-tail)))
                 (if (string-empty-p tail) (string-trim event) tail))))))

(defun piano--start-synth ()
  "Start FluidSynth unless it is already running.

Signal a `user-error' when FluidSynth exits during startup, e.g. on
a bad SoundFont or an unusable audio driver."
  (unless (and piano--synth-process
               (process-live-p piano--synth-process))
    (let ((program (or (executable-find piano-synth-program)
                       (user-error "FluidSynth not found (%S); install it first"
                                   piano-synth-program)))
          (soundfont (or (piano--soundfont)
                         (user-error "SoundFont not found; install one or \
set `piano-soundfont'"))))
      (with-current-buffer (get-buffer-create " *piano-fluidsynth-errors*")
        (erase-buffer))
      (let ((piano--synth-starting t)
            (process
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
              :stderr (get-buffer-create " *piano-fluidsynth-errors*")
              :sentinel #'piano--synth-sentinel)))
        (setq piano--synth-process process)
        ;; Give it a moment to start: FluidSynth exits immediately on
        ;; a bad SoundFont or unusable audio driver.
        (accept-process-output process 0.3)
        (unless (process-live-p process)
          (setq piano--synth-process nil)
          (user-error "FluidSynth exited at startup: %s"
                      (or (piano--synth-error-tail) "unknown error")))))))

(defun piano--start ()
  "Start the synthesizer and the key reader."
  (piano--start-synth)
  (piano--start-helper)
  (add-hook 'window-buffer-change-functions #'piano--window-changed)
  ;; Record the focus state now: the hook above only fires on changes.
  (setq piano--focused-p (piano--helper-active-p)))

(defun piano--stop ()
  "Stop the key reader, release all notes, stop the synthesizer."
  (let ((piano--stopping t))
    (remove-hook 'window-buffer-change-functions #'piano--window-changed)
    (when (process-live-p piano--keys-process)
      (delete-process piano--keys-process))
    (setq piano--keys-process nil
          piano--focused-p nil
          piano--piano-buffer nil)
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
         ;; A failed start must not leak the synthesizer process.
         (setq piano-mode nil)
         (piano--stop)
         (signal (car error) (cdr error))))
    (piano--stop)))

;;;###autoload
(defun piano-play ()
  "Open a dedicated piano buffer and start playing."
  (interactive)
  (let ((buffer (get-buffer-create "*piano*")))
    (pop-to-buffer buffer)
    (setq piano--piano-buffer buffer)
    (add-hook 'kill-buffer-hook #'piano--kill-buffer nil t)
    (with-current-buffer buffer
      (unless (derived-mode-p 'special-mode)
        (special-mode)))
    ;; Enable the mode first: the device prompt (if any) must finish
    ;; before the header below shows which device is active.
    (piano-mode 1)
    (with-current-buffer buffer
      (piano--show-keys))))

(defun piano--show-keys ()
  "Insert the current key layout into the current buffer."
  (let ((inhibit-read-only t))
    (erase-buffer)
    (insert "piano-mode — the keys below are read globally by piano-keys\n"
            (format "device: %s\n"
                    (if (and (stringp piano-device)
                             (not (string-empty-p piano-device)))
                        piano-device
                      "auto-detect"))
            "white keys: "
            (mapconcat (lambda (binding) (upcase (car binding)))
                       piano-keys " ")
            "\n"
            "M-x piano-mode stops the instrument; kill this buffer to quit\n")
    (add-text-properties (point-min) (point-max)
                         '(piano-layout t))))

(defun piano-test-note (&optional midi)
  "Play MIDI note MIDI (default 60) briefly, to test the sound path."
  (interactive)
  (let ((note (or midi 60)))
    (unless (and piano--synth-process
                 (process-live-p piano--synth-process))
      (piano--start-synth))
    (piano--note-on note)
    (run-at-time 0.8 nil #'piano--note-off note)))

(add-hook 'kill-emacs-hook #'piano--stop)

(provide 'piano-mode)
;;; piano-mode.el ends here
