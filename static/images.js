/* This file is licensed under AGPL-3.0 */
const uploadForm = document.getElementById('upload-form');
const uploadInput = document.getElementById('upload-file-input');

const dropZone = document.getElementById('file-upload-drop-zone');
let lastDraggedTarget = null;

class BulkFilesOperations {
    static checkedSelector = '.entry:not(.hidden) > .file-bulk > input[type="checkbox"]';

    constructor(
        table,
        {
            deleteFiles = null,
            deleteModal = null,
            totalFileCount = null,
            selectedFileCount = null,
        } = {},
    ) {
        this.parent = table;
        this.checkboxAnchor = null;
        this.bulkCheck = table?.querySelector('.bulk-check');
        this.deleteFilesButton = deleteFiles;
        this.totalFileCount = totalFileCount;
        this.selectedFileCount = selectedFileCount;
        this.deleteModal = deleteModal;
        // These require hardcoded IDs since forms tend have IDs
        this.confirmDeleteButton = document.getElementById('confirm-delete');

        this.bulkCheck?.addEventListener('click', () => this.processBulkCheck());

        this.confirmDeleteButton?.addEventListener('click', (e) => {
            e.preventDefault();
            let form = this.deleteModal?.querySelector('form');
            if(form?.reportValidity()) {
                this.deleteFiles();
                form.reset();
            }
        });

        this.deleteModal?.querySelector('button[formmethod=dialog]')?.addEventListener('click', (e) => this.closeModal(e, this.deleteModal));

        this.deleteFilesButton?.addEventListener('click', () => this.showConfirmFileModal(this.deleteModal));

        document.addEventListener('entries-filtered', () => this.setCheckboxState());
        this.parent?.querySelectorAll('.file-bulk > input[type="checkbox"]').forEach(ch => {
            ch.addEventListener('click', (e) => {
                this.handleCheckboxClick(e);
                this.setCheckboxState();
            });
        });
    }

    closeModal(event, modal) {
        event.preventDefault();
        modal.close();
    }

    showConfirmFileModal(modal) {
        if(!modal) return;
        let files = this.getSelectedFiles();
        let span = modal.querySelector('span');
        span.textContent = files.length === 1 ? '1 file' : files.length === 0 ? `all` : `${files.length} files`;
        modal.showModal();
    }

    processBulkCheck() {
        let indeterminate = this.bulkCheck.getAttribute('tribool') === 'yes';
        let checked = indeterminate ? false : this.bulkCheck.checked;
        let selected = [...this.parent.querySelectorAll(this.constructor.checkedSelector)];
        for(const ch of selected) {
            ch.checked = checked;
        }

        this.updateFileCounts(checked ? selected.length : 0);
        if (indeterminate) {
            this.bulkCheck.checked = false;
            this.bulkCheck.indeterminate = false;
            this.bulkCheck.removeAttribute('tribool');
        }
    }

    handleCheckboxClick(e) {
        if(e.ctrlKey) {
            return;
        }
        if(!e.shiftKey) {
            this.checkboxAnchor = e.target;
            return;
        }
        let activeCheckboxes = [...this.parent.querySelectorAll(this.constructor.checkedSelector)];
        let startIndex = activeCheckboxes.indexOf(this.checkboxAnchor);
        let endIndex = activeCheckboxes.indexOf(e.target);
        if(startIndex === endIndex || startIndex === -1 || endIndex === -1) {
            return;
        }

        if(startIndex > endIndex) {
            let temp = endIndex;
            endIndex = startIndex;
            startIndex = temp;
        }

        for(let i = startIndex; i <= endIndex; ++i) {
            let cb = activeCheckboxes[i];
            cb.checked = true;
        }
    }

    getSelectedFiles() {
        return [...this.parent.querySelectorAll(this.constructor.checkedSelector + ':checked')].map(e => {
            return e.parentElement.parentElement.querySelector('.file-name');
        });
    }

    updateFileCounts(checked = null) {
        if(checked === null) {
            let checkboxes = [...this.parent.querySelectorAll(this.constructor.checkedSelector)];
            checked = checkboxes.reduce((prev, el) => prev + el.checked, 0);
        }
        if(this.selectedFileCount) {
            this.selectedFileCount.classList.toggle('hidden', checked === 0);
            this.selectedFileCount.textContent = `${checked} file${checked !== 1 ? 's' : ''} selected`;
        }

        if(this.totalFileCount) {
            let total = [...this.parent.querySelectorAll('.entry:not(.hidden)')].length;
            this.totalFileCount.textContent = `${total} file${total !== 1 ? 's' : ''}`;
        }
    }

    removeCheckedFiles() {
        this.parent.querySelectorAll(this.constructor.checkedSelector + ':checked').forEach(e => {
            let parent = e.parentElement.parentElement;
            return parent.parentElement.removeChild(parent);
        });
        this.setCheckboxState();
    }

    setCheckboxState() {
        let checkboxes = [...this.parent.querySelectorAll(this.constructor.checkedSelector)];
        let checked = checkboxes.reduce((prev, el) => prev + el.checked, 0);
        let nothingChecked = checked === 0;
        this.updateFileCounts(checked);

        if(nothingChecked) {
            this.bulkCheck.checked = false;
            this.bulkCheck.indeterminate = false;
            this.bulkCheck.removeAttribute('tribool');
        }
        else if(checked === checkboxes.length) {
            this.bulkCheck.indeterminate = false;
            this.bulkCheck.removeAttribute('tribool');
            this.bulkCheck.checked = true;
        } else {
            this.bulkCheck.indeterminate = true;
            this.bulkCheck.setAttribute('tribool', 'yes');
            this.bulkCheck.checked = false;
        }
    }

    async deleteFiles() {
        let files = this.getSelectedFiles().map(e => e.textContent);
        let payload = {files};

        let js = await callApi(`/images/bulk`, {
            method: 'DELETE',
            headers: {
                'content-type': 'application/json',
            },
            body: JSON.stringify(payload)
        });

        if(js === null) {
            return;
        }

        let total = js.success + js.failed;
        showAlert({level: 'success', content: `Successfully deleted ${js.success}/${total} file${total === 1 ? "" : "s"}`});

        this.deleteModal.close();
        this.removeCheckedFiles();
    }
}

const fileExtension = (name) => name.slice((name.lastIndexOf('.') - 1 >>> 0) + 2);
const allowedExtensions = ["apng", "png", "jpg", "jpeg", "gif", "avif"];

function filterValidFileList(files) {
    let filtered = Array.from(files).filter(f => allowedExtensions.includes(fileExtension(f.name)));
    const dt = new DataTransfer();
    filtered.forEach(f => dt.items.add(f));
    return dt.files;
}

function showModalAlert(modal, {level, content}) {
    if(modal) {
        let alert = createAlert({level, content});
        let el = modal.querySelector('h1');
        el.parentNode.insertBefore(alert, el.nextSibling);
    } else {
        showAlert({level, content});
    }
}

function modalAlertHook(modal) {
    let el = modal.querySelector('h1');
    return (e) => el.parentNode.insertBefore(e, el.nextSibling);
}

uploadInput?.addEventListener('change', () => {
    uploadForm.submit();
});

const __bulk = new BulkFilesOperations(document.querySelector('.files'), {
    deleteFiles: document.getElementById('delete-files'),
    deleteModal: document.getElementById('confirm-delete-modal'),
    totalFileCount: document.getElementById('total-file-count'),
    selectedFileCount: document.getElementById('selected-file-count'),
});

if(__bulk && uploadInput !== null) {
    document.addEventListener('keyup', (e) => {
        if(e.key === 'Delete') {
            __bulk?.deleteFilesButton?.click();
        }
    });
}

if (uploadInput !== null) {
    window.addEventListener('dragenter', (e) => {
        lastDraggedTarget = e.target;
        dropZone.classList.add('dragged');
    });

    window.addEventListener('dragleave', (e) => {
        if (e.target === lastDraggedTarget || e.target === document) {
            dropZone.classList.remove('dragged');
        }
    });

    window.addEventListener('dragover', (e) => {
        e.preventDefault();
    });

    window.addEventListener('drop', (e) => {
        e.preventDefault();
        dropZone.classList.remove('dragged');
        let files = filterValidFileList(e.dataTransfer.files);
        if(files.length > 0) {
            uploadInput.files = files;
            uploadForm.submit();
        }
    });
}
