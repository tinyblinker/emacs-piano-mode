;;; piano-mode-test.el --- Tests for piano-mode  -*- lexical-binding: t; -*-

;; Run with:
;;   emacs --batch -Q -L . -l test/piano-mode-test.el \
;;         -f ert-run-tests-batch-and-exit

(require 'ert)
(require 'cl-lib)
(require 'piano-mode)

(defconst piano-test-dir
  (file-name-directory (or load-file-name buffer-file-name)))

(defconst piano-test-bin
  (expand-file-name "fixtures/bin" piano-test-dir))

(defconst piano-test-soundfont
  (expand-file-name "fixtures/dummy-soundfont" piano-test-dir))

(defun piano-test-wait-for (predicate &optional seconds)
  "Wait up to SECONDS (default 5) for PREDICATE to become non-nil.
Return PREDICATE's final value."
  (let ((deadline (+ (float-time) (or seconds 5))))
    (while (and (not (funcall predicate)) (< (float-time) deadline))
      (sit-for 0.05))
    (funcall predicate)))

(defun piano-test-slurp (file)
  "Return the contents of FILE."
  (with-temp-buffer
    (insert-file-contents file)
    (buffer-string)))

(defmacro piano-test-environment (&rest body)
  "Run BODY with fake helper/synthesizer programs and clean state."
  `(let ((piano-test-log (make-temp-file "piano-synth" nil ".log")))
     (unwind-protect
         (let ((exec-path (cons piano-test-bin exec-path))
               (piano-keys-program "piano-keys")
               (piano-synth-program "fluidsynth")
               (piano-soundfont piano-test-soundfont)
               (piano-poll-ms 4)
               (process-environment
                (cons (format "PIANO_SYNTH_LOG=%s" piano-test-log)
                      process-environment)))
           (cl-letf (((symbol-function 'piano--stable-device-path)
                      (lambda (path) path)))
             (clrhash piano--notes)
             ,@body))
       (delete-file piano-test-log))))

(ert-deftest piano-test-helper-pipeline ()
  "A down/up pair from piano-keys becomes noteon/noteoff."
  (piano-test-environment
    (with-temp-buffer
      (set-window-buffer (selected-window) (current-buffer))
      (let ((piano--buffer (current-buffer)))
        (piano-mode 1)
        (unwind-protect
            (progn
              (should (piano-test-wait-for
                       (lambda ()
                         (let ((log (piano-test-slurp piano-test-log)))
                           (and (string-match-p "noteon 0 60 100" log)
                                (string-match-p "noteoff 0 60" log))))))
              (should (= 0 (hash-table-count piano--notes))))
          (piano-mode -1))))))

(ert-deftest piano-test-stop-kills-processes ()
  "Disabling the mode stops both helper processes."
  (piano-test-environment
    (with-temp-buffer
      (piano-mode 1)
      (unwind-protect
          (progn
            (should (process-live-p piano--keys-process))
            (should (process-live-p piano--synth-process)))
        (piano-mode -1))
      (should-not (process-live-p piano--keys-process))
      (should-not (process-live-p piano--synth-process)))))

(ert-deftest piano-test-lines-are-dispatched ()
  "down/up lines map to noteon/noteoff; unknown keys are ignored."
  (let ((calls '()))
    (clrhash piano--notes)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls)))
              ((symbol-function 'piano--buffer-selected-p)
               (lambda () t)))
      (piano--handle-keys-line "down A")
      (piano--handle-keys-line "up A")
      (piano--handle-keys-line "down Q")
      (piano--handle-keys-line "garbage"))
    (should (member "noteon 0 60 100\n" calls))
    (should (member "noteoff 0 60\n" calls))
    (should (= 0 (hash-table-count piano--notes)))))

(ert-deftest piano-test-key-names-are-case-insensitive ()
  "Uppercase `piano-keys' entries match the reader's uppercase names."
  (let ((piano-keys '(("1" . 36) ("P" . 59) ("O" . 60) ("/" . 95)))
        (calls '()))
    (clrhash piano--notes)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls)))
              ((symbol-function 'piano--buffer-selected-p)
               (lambda () t)))
      (dolist (key '("1" "P" "O" "/"))
        (piano--handle-keys-line (format "down %s" key))
        (piano--handle-keys-line (format "up %s" key))))
    (should (member "noteon 0 36 100\n" calls))
    (should (member "noteon 0 59 100\n" calls))
    (should (member "noteon 0 60 100\n" calls))
    (should (member "noteon 0 95 100\n" calls))
    (should (= 0 (hash-table-count piano--notes)))))

(ert-deftest piano-test-release-without-focus ()
  "A key-up releases a note after the piano buffer loses focus."
  (let ((calls '())
        (selected t))
    (clrhash piano--notes)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls)))
              ((symbol-function 'piano--buffer-selected-p)
               (lambda () selected)))
      (piano--handle-keys-line "down A")
      (setq selected nil)
      (piano--handle-keys-line "up A"))
    (should (member "noteon 0 60 100\n" calls))
    (should (member "noteoff 0 60\n" calls))
    (should (= 0 (hash-table-count piano--notes)))))

(ert-deftest piano-test-transpose ()
  "`piano-transpose' shifts the MIDI note."
  (let ((calls '())
        (piano-transpose 2))
    (clrhash piano--notes)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls)))
              ((symbol-function 'piano--buffer-selected-p)
               (lambda () t)))
      (piano--handle-keys-line "down A"))
    (should (member "noteon 0 62 100\n" calls))
    (should (member 62 (hash-table-keys piano--notes)))))

(ert-deftest piano-test-keys-argument ()
  "The --keys argument carries every mapped key, upcased."
  (should (equal (piano--helper-keys-argument)
                 "A,W,S,E,D,F,T,G,Y,H,U,J,K,O,L,P,;")))

(ert-deftest piano-test-keys-custom-set ()
  "Changing `piano-keys' restarts a live reader and leaves the map alone."
  (let ((old-keys piano-keys)
        (old-map piano-mode-map)
        (restarted nil)
        (piano--keys-process (start-process "piano-test-keys" nil "sleep" "5")))
    (unwind-protect
        (cl-letf (((symbol-function 'piano--restart-helper)
                   (lambda () (setq restarted t))))
          (customize-set-variable 'piano-keys '(("P" . 72)))
          (should (equal (piano--helper-keys-argument) "P"))
          ;; The map object must stay the one `define-minor-mode'
          ;; recorded in `minor-mode-map-alist'.
          (should (eq old-map piano-mode-map))
          (should (eq (cdr (assq 'piano-mode minor-mode-map-alist))
                      piano-mode-map))
          (should (eq (lookup-key piano-mode-map "p") #'ignore))
          (should restarted)
          ;; Restore the default while the restart helper is still
          ;; stubbed, then clean up the dummy process.
          (customize-set-variable 'piano-keys old-keys))
      (delete-process piano--keys-process))))

(ert-deftest piano-test-printable-keys-are-ignored ()
  "Digits and special-mode commands are ignored in the piano buffer."
  (with-temp-buffer
    (special-mode)
    (setq piano-mode t)
    (dolist (key (list "0" "5" "q" "g" ";"
                       (kbd "SPC") (kbd "TAB") (kbd "RET") (kbd "DEL")))
      (should (eq (key-binding key) #'ignore)))))

(ert-deftest piano-test-find-keys-program-repo ()
  "A sibling build inside the repository is found without PATH."
  (let ((dir (make-temp-file "piano-repo" t)))
    (unwind-protect
        (let* ((binary (expand-file-name "piano_core/target/debug/piano-keys" dir))
               (piano--load-dir dir))
          (make-directory (expand-file-name "piano_core/target/debug" dir) t)
          (write-region "" nil binary)
          (set-file-modes binary #o755)
          (cl-letf (((symbol-function 'executable-find)
                     (lambda (_name) nil)))
            (should (equal (piano--find-keys-program) binary))))
      (delete-directory dir t))))

(ert-deftest piano-test-load-dir-source ()
  "A source or byte-code file yields its own directory."
  (let ((load-file-name "/tmp/piano-mode.el"))
    (should (equal (piano--guess-load-dir) "/tmp/"))))

(ert-deftest piano-test-load-dir-eln ()
  "A native-compiled file falls back to `locate-library'."
  (let ((load-file-name "/tmp/eln-cache/piano-mode.eln"))
    (cl-letf (((symbol-function 'locate-library)
               (lambda (_library) "/repo/piano-mode.elc")))
      (should (equal (piano--guess-load-dir) "/repo/")))))

(ert-deftest piano-test-build-keys-program ()
  "A missing bundled reader is built on demand."
  (let ((directory (make-temp-file "piano-repo" t))
        (piano-keys-program "piano-keys")
        (piano-auto-build t)
        (piano--load-dir nil))
    (unwind-protect
        (progn
          (make-directory (expand-file-name "piano_core" directory) t)
          (write-region "" nil
                        (expand-file-name "piano_core/Cargo.toml" directory))
          (setq piano--load-dir directory)
          (cl-letf (((symbol-function 'executable-find)
                     (lambda (name)
                       (if (equal name "cargo") "/fake/cargo" nil)))
                    ((symbol-function 'call-process)
                     (lambda (_program _infile _destination _display &rest args)
                       (should (equal args
                                      (list "build" "--release" "--locked"
                                            "--manifest-path"
                                            (expand-file-name
                                             "piano_core/Cargo.toml" directory))))
                       (let ((binary
                              (expand-file-name
                               "piano_core/target/release/piano-keys"
                               directory)))
                         (make-directory (file-name-directory binary) t)
                         (write-region "" nil binary)
                         (set-file-modes binary #o755))
                       0)))
            (should (equal
                     (piano--ensure-keys-program)
                     (expand-file-name
                      "piano_core/target/release/piano-keys" directory)))))
      (delete-directory directory t))))

(ert-deftest piano-test-soundfont-next-to-package ()
  "A SoundFont next to the loaded package is found."
  (let ((directory (make-temp-file "piano-repo" t))
        (piano-soundfont nil)
        (piano--load-dir nil)
        (process-environment
         (delete "PIANO_SOUNDFONT" process-environment)))
    (unwind-protect
        (progn
          (make-directory (expand-file-name "piano_core/src" directory) t)
          (write-region "" nil
                        (expand-file-name
                         "piano_core/src/TimGM6mb.sf2" directory))
          (setq piano--load-dir directory)
          (should (equal (piano--soundfont)
                         (expand-file-name
                          "piano_core/src/TimGM6mb.sf2" directory))))
      (delete-directory directory t))))

(ert-deftest piano-test-parse-devices ()
  "Keyboard lines in --list-devices output become (LABEL . PATH) pairs."
  (should
   (equal
    (piano--parse-devices
     (concat "/dev/input/event1\tFake Keyboard\tyes\n"
             "/dev/input/event2\tFake Mouse\tno\n"
             "/dev/input/event3\t\tno\n"))
    '(("Fake Keyboard (/dev/input/event1)" . "/dev/input/event1")))))

(ert-deftest piano-test-stable-device-path ()
  "A by-id symlink is preferred over a raw event path."
  (cl-letf (((symbol-function 'directory-files)
             (lambda (directory &optional _full _match)
               (pcase directory
                 ("/dev/input/by-id" '("/dev/input/by-id/kbd"))
                 ("/dev/input/by-path" '("/dev/input/by-path/kbd"))
                 (_ nil))))
            ((symbol-function 'file-equal-p)
             (lambda (first _second)
               (equal first "/dev/input/by-id/kbd"))))
    (should (equal (piano--stable-device-path "/dev/input/event9")
                   "/dev/input/by-id/kbd"))))

(ert-deftest piano-test-select-device-single ()
  "A single candidate device is picked without prompting."
  (let ((piano-device nil))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("Only (/dev/input/event3)" . "/dev/input/event3"))))
              ((symbol-function 'piano--stable-device-path)
               (lambda (path) path)))
      (should (equal (piano--select-device "piano-keys")
                     "/dev/input/event3")))))

(ert-deftest piano-test-select-device-prompt ()
  "With several candidates the completion choice wins and is saved."
  (let ((piano-device nil)
        (saved 'unset))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("Keyboard B (/dev/input/event5)" . "/dev/input/event5")
                   ("Keyboard A (/dev/input/event3)" . "/dev/input/event3"))))
              ((symbol-function 'completing-read)
               (lambda (_prompt choices &rest _args)
                 (should (member "Auto-detect (first match)" choices))
                 "Keyboard A (/dev/input/event3)"))
              ((symbol-function 'customize-save-variable)
               (lambda (_symbol value) (setq saved value)))
              ((symbol-function 'piano--stable-device-path)
               (lambda (path) path)))
      (should (equal (piano--select-device "piano-keys")
                     "/dev/input/event3"))
      (should (equal saved "/dev/input/event3")))))

(ert-deftest piano-test-select-device-pinned ()
  "A saved `piano-device' is used when it is still a keyboard."
  (let ((piano-device "/dev/input/event9"))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("Keyboard (/dev/input/event9)" . "/dev/input/event9"))))
              ((symbol-function 'file-equal-p)
               (lambda (_first _second) t))
              ((symbol-function 'piano--stable-device-path)
               (lambda (path) path)))
      (should (equal (piano--select-device "piano-keys")
                     "/dev/input/event9")))))

(ert-deftest piano-test-select-device-stale ()
  "A saved device that is no longer a keyboard is replaced."
  (let ((piano-device "/dev/input/event9"))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("Only (/dev/input/event3)" . "/dev/input/event3"))))
              ((symbol-function 'file-equal-p)
               (lambda (_first _second) nil))
              ((symbol-function 'piano--stable-device-path)
               (lambda (path) path)))
      (should (equal (piano--select-device "piano-keys")
                     "/dev/input/event3"))
      (should-not piano-device))))

(ert-deftest piano-test-restart-helper-after-exit ()
  "Restarting starts the reader again when the mode is active."
  (piano-test-environment
    (with-temp-buffer
      (set-window-buffer (selected-window) (current-buffer))
      (let ((piano--buffer (current-buffer)))
        (piano-mode 1)
        (unwind-protect
            (progn
              (set-process-sentinel piano--keys-process #'ignore)
              (delete-process piano--keys-process)
              (setq piano--keys-process nil)
              (piano--restart-helper)
              (should (process-live-p piano--keys-process)))
          (piano-mode -1))))))

(ert-deftest piano-test-soundfont-unreadable ()
  "A configured but missing SoundFont gives a clear error."
  (let ((piano-soundfont "/definitely/missing.sf2"))
    (should-error (piano--soundfont) :type 'user-error)))

(ert-deftest piano-test-synth-missing-program ()
  "A missing FluidSynth gives a clear error."
  (let ((piano-synth-program "piano-definitely-missing")
        (piano--synth-process nil))
    (cl-letf (((symbol-function 'executable-find)
               (lambda (_name) nil)))
      (should-error (piano--start-synth) :type 'user-error))))

(ert-deftest piano-test-ensure-keys-program-error ()
  "A missing reader gives a clear error when auto-build is off."
  (let ((piano-keys-program "piano-definitely-missing")
        (piano-auto-build nil)
        (piano--load-dir nil))
    (cl-letf (((symbol-function 'executable-find)
               (lambda (_name) nil)))
      (should-error (piano--ensure-keys-program) :type 'user-error))))

(ert-deftest piano-test-list-devices-error ()
  "A reader that cannot list devices gives a clear error."
  (let ((script (make-temp-file "piano-keys-bad" nil ".sh")))
    (unwind-protect
        (progn
          (with-temp-file script
            (insert "#!/bin/sh\necho 'cannot list /dev/input' >&2\nexit 1\n"))
          (set-file-modes script #o755)
          (should-error (piano--list-devices script) :type 'user-error))
      (delete-file script))))

(ert-deftest piano-test-start-helper-exits ()
  "A reader that exits immediately gives a clear startup error."
  (let ((script (make-temp-file "piano-keys-exit" nil ".sh"))
        (piano-keys-program nil)
        (piano-auto-build nil)
        (piano-device nil))
    (unwind-protect
        (progn
          (with-temp-file script
            (insert "#!/bin/sh\n"
                    "if [ \"$1\" = \"--list-devices\" ]; then\n"
                    "  printf '/dev/input/event1\\tFake\\tyes\\n'\n"
                    "  exit 0\n"
                    "fi\n"
                    "echo 'bad device' >&2\n"
                    "exit 1\n"))
          (set-file-modes script #o755)
          (setq piano-keys-program script)
          (cl-letf (((symbol-function 'piano--stable-device-path)
                     (lambda (path) path)))
            (should-error (piano--start-helper) :type 'user-error)))
      (delete-file script))))

(ert-deftest piano-test-helper-active-gate ()
  "Key events are gated on the piano buffer existing and being focused."
  (with-temp-buffer
    (set-window-buffer (selected-window) (current-buffer))
    (let ((piano--buffer nil))
      (should-not (piano--buffer-selected-p))       ; no piano buffer yet
      (setq piano--buffer (current-buffer))
      (should (piano--buffer-selected-p))           ; piano buffer focused
      (set-window-buffer (selected-window) (get-buffer-create "*scratch*"))
      (should-not (piano--buffer-selected-p)))))    ; focus moved away

(ert-deftest piano-test-selection-change-releases-notes ()
  "Leaving the piano buffer releases notes that are still sounding."
  (let ((piano--buffer (current-buffer))
        (calls '()))
    (clrhash piano--notes)
    (puthash 60 t piano--notes)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls))))
      (set-window-buffer (selected-window) (get-buffer-create "*scratch*"))
      (piano--selection-changed (selected-window)))
    (should (member "noteoff 0 60\n" calls))
    (should (= 0 (hash-table-count piano--notes)))))

(ert-deftest piano-test-select-device-auto ()
  "Choosing auto-detect yields nil and saves nothing."
  (let ((piano-device nil)
        (saved 'unset))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("A (/dev/input/event3)" . "/dev/input/event3")
                   ("B (/dev/input/event5)" . "/dev/input/event5"))))
              ((symbol-function 'completing-read)
               (lambda (_prompt _choices &rest _args)
                 "Auto-detect (first match)"))
              ((symbol-function 'customize-save-variable)
               (lambda (_symbol value) (setq saved value)))
              ((symbol-function 'piano--stable-device-path)
               (lambda (path) path)))
      (should (equal (piano--select-device "piano-keys") nil))
      (should (eq saved 'unset)))))

(ert-deftest piano-test-restart-helper ()
  "Restarting the reader picks up a changed `piano-device'."
  (piano-test-environment
    (with-temp-buffer
      (set-window-buffer (selected-window) (current-buffer))
      (let ((piano--buffer (current-buffer)))
        (piano-mode 1)
        (unwind-protect
            (let ((old piano--keys-process))
              (should (process-live-p old))
              (setq piano-device "/dev/input/event1")
              (piano--restart-helper)
              (should (process-live-p piano--keys-process))
              (should-not (eq old piano--keys-process))
              (should (member "--device" (process-command piano--keys-process)))
              (should (member "/dev/input/event1"
                              (process-command piano--keys-process))))
          (piano-mode -1))))))

(ert-deftest piano-test-select-device-command ()
  "The select-device command saves the pick."
  (let ((saved 'unset)
        (piano-device nil))
    (cl-letf (((symbol-function 'piano--ensure-keys-program)
               (lambda () "piano-keys"))
              ((symbol-function 'piano--select-device)
               (lambda (_program) "/dev/input/event5"))
              ((symbol-function 'customize-save-variable)
               (lambda (_symbol value) (setq saved value))))
      (piano-select-device)
      (should (equal saved "/dev/input/event5")))))

;;; piano-mode-test.el ends here
