use super::sii_reader::SiiTaskError;
use super::AlStateTransferError;
use super::{al_state_transfer::AlStateTransfer, sii_reader::SiiReader};
use super::super::{EtherCatSystemTime, ReceivedData};
use crate::cyclic_task::CyclicProcess;
use crate::error::EcError;
use super::super::interface::*;
use crate::register::sii::*;
use crate::register::{
        CyclicOperationStartTime, DcActivation, Latch0NegativeEdgeValue, Latch0PositiveEdgeValue,
        Latch1NegativeEdgeValue, Latch1PositiveEdgeValue, LatchEdge, LatchEvent, PdiControl,
        Sync0CycleTime, Sync1CycleTime,
        DlControl, DlInformation, DlStatus, DlUserWatchDog, FixedStationAddress, FmmuRegister,
        RxErrorCounter, SyncManagerActivation, SyncManagerChannelWatchDog, SyncManagerControl,
        SyncManagerStatus, WatchDogDivider,
};
use crate::slave_network::SyncManagerType;
use crate::slave_network::{AlState, SlaveInfo, SyncManager};
use crate::util::const_max;
use bit_field::BitField;

#[derive(Debug, Clone)]
pub enum SlaveInitializerError {
    AlStateTransition(AlStateTransferError),
    SiiRead(SiiTaskError),
    FailedToLoadEEPROM,
}

impl From<SlaveInitializerError> for EcError<SlaveInitializerError> {
    fn from(err: SlaveInitializerError) -> Self {
        Self::TaskSpecific(err)
    }
}

impl From<EcError<AlStateTransferError>> for EcError<SlaveInitializerError> {
    fn from(err: EcError<AlStateTransferError>) -> Self {
        match err {
            EcError::TaskSpecific(err) => {
                EcError::TaskSpecific(SlaveInitializerError::AlStateTransition(err))
            }
            EcError::Interface(e) => EcError::Interface(e),
            EcError::LostPacket => EcError::LostPacket,
            EcError::UnexpectedCommand => EcError::UnexpectedCommand,
            EcError::UnexpectedWkc(wkc) => EcError::UnexpectedWkc(wkc),
        }
    }
}

impl From<EcError<SiiTaskError>> for EcError<SlaveInitializerError> {
    fn from(err: EcError<SiiTaskError>) -> Self {
        match err {
            EcError::TaskSpecific(err) => {
                EcError::TaskSpecific(SlaveInitializerError::SiiRead(err))
            }
            EcError::Interface(e) => EcError::Interface(e),
            EcError::LostPacket => EcError::LostPacket,
            EcError::UnexpectedCommand => EcError::UnexpectedCommand,
            EcError::UnexpectedWkc(wkc) => EcError::UnexpectedWkc(wkc),
        }
    }
}

#[derive(Debug)]
enum State {
    Idle,
    Error(EcError<SlaveInitializerError>),
    SetLoopPort,
    RequestInitState(bool),
    ResetErrorCount,
    SetWatchDogDivider,
    DisableDlWatchDog,
    DisableSmWatchDog,
    CheckDlStatus,
    CheckDlInfo,
    ClearFmmu(u16),
    ClearSm(u16),
    GetVenderID(bool),
    GetProductCode(bool),
    GetRevision(bool),
    GetProtocol(bool),
    GetRxMailboxSize(bool),
    GetRxMailboxOffset(bool),
    GetTxMailboxSize(bool),
    GetTxMailboxOffset(bool),
    SetSmControl(usize),
    SetSmActivation(usize),
    SetStationAddress,
    CheckPdiControl,
    ClearDcActivation,
    ClearCyclicOperationStartTime,
    ClearSync0CycleTime,
    ClearSync1CycleTime,
    //ClearLatchEdge,
    //ClearLatchEvent,
    //ClearLatch0PositiveEdgeValue,
    //ClearLatch0NegativeEdgeValue,
    //ClearLatch1PositiveEdgeValue,
    //ClearLatch1NegativeEdgeValue,
    Complete,
}

#[derive(Debug)]
pub struct SlaveInitializer {
    inner: InnerFunction,
    slave_address: SlaveAddress,
    state: State,
    command: Command,
    buffer: [u8; buffer_size()],
    slave_info: Option<SlaveInfo>,
}

impl SlaveInitializer {
    pub fn new() -> Self {
        Self {
            inner: InnerFunction::This,
            slave_address: SlaveAddress::SlavePosition(0),
            state: State::Idle,
            command: Command::default(),
            buffer: [0; buffer_size()],
            slave_info: None,
        }
    }

    pub fn start(&mut self, slave_position: u16) {
        self.slave_address = SlaveAddress::SlavePosition(slave_position);
        self.state = State::SetLoopPort;
        self.slave_info = Some(SlaveInfo::default());
        if let Some(slave) = self.slave_info.as_mut() {
            slave.mailbox_count.set(1)
        }
    }

    pub fn wait(&mut self) -> Option<Result<Option<SlaveInfo>, EcError<SlaveInitializerError>>> {
        match &self.state {
            State::Complete => Some(Ok(core::mem::take(&mut self.slave_info))),
            State::Error(err) => Some(Err(err.clone())),
            _ => None,
        }
    }
}

impl CyclicProcess for SlaveInitializer {
    fn next_command(&mut self, sys_time: EtherCatSystemTime) -> Option<(Command, &[u8])> {
        log::info!("send {:?}", self.state);

        let command_and_data = match self.state {
            State::Idle => None,
            State::Error(_) => None,
            State::Complete => None,
            State::SetLoopPort => {
                let command = Command::new_write(self.slave_address.into(), DlControl::ADDRESS);
                self.buffer.fill(0);
                // ループポートを設定する。
                // ・EtherCat以外のフレームを削除する。
                // ・ソースMACアドレスを変更して送信する。
                // ・ポートを自動開閉する。
                let mut dl_control = DlControl(&mut self.buffer);
                dl_control.set_forwarding_rule(true);
                dl_control.set_tx_buffer_size(7);
                dl_control.set_enable_alias_address(false);
                Some((command, &self.buffer[..DlControl::SIZE]))
            }
            State::RequestInitState(is_first) => {
                self.inner.into_al_state_transfer();
                let al_transfer = self.inner.al_state_transfer().unwrap();
                if is_first {
                    al_transfer.start(self.slave_address.into(), AlState::Init);
                }
                //al_transfer.next_command(desc, sys_time)
                al_transfer.next_command(sys_time)
            }
            State::ResetErrorCount => {
                let command =
                    Command::new_write(self.slave_address.into(), RxErrorCounter::ADDRESS);
                self.buffer.fill(0);
                Some((command, &self.buffer[..RxErrorCounter::SIZE]))
            }
            State::SetWatchDogDivider => {
                let command =
                    Command::new_write(self.slave_address.into(), WatchDogDivider::ADDRESS);
                self.buffer.fill(0);
                let mut watchdog_div = WatchDogDivider(&mut self.buffer);
                watchdog_div.set_watch_dog_divider(2498); //100us(default)
                Some((command, &self.buffer[..WatchDogDivider::SIZE]))
            }
            State::DisableDlWatchDog => {
                let command =
                    Command::new_write(self.slave_address.into(), DlUserWatchDog::ADDRESS);
                self.buffer.fill(0);
                // disable dl watch dog
                Some((command, &self.buffer[..DlUserWatchDog::SIZE]))
            }
            State::DisableSmWatchDog => {
                let command = Command::new_write(
                    self.slave_address.into(),
                    SyncManagerChannelWatchDog::ADDRESS,
                );
                self.buffer.fill(0);
                // disable sm watch dog
                Some((command, &self.buffer[..SyncManagerChannelWatchDog::SIZE]))
            }
            State::CheckDlStatus => {
                // ポートと、EEPROMのロード状況を確認する。
                let command = Command::new_read(self.slave_address.into(), DlStatus::ADDRESS);
                self.buffer.fill(0);
                Some((command, &self.buffer[..DlStatus::SIZE]))
            }
            State::CheckDlInfo => {
                // 各種サポート状況の確認
                let command = Command::new_read(self.slave_address.into(), DlInformation::ADDRESS);
                self.buffer.fill(0);
                Some((command, &self.buffer[..DlInformation::SIZE]))
            }
            State::ClearFmmu(count) => {
                let command = Command::new_write(
                    self.slave_address.into(),
                    FmmuRegister::ADDRESS + count * 0x10,
                );
                self.buffer.fill(0);
                // disable dl watch dog
                Some((command, &self.buffer[..FmmuRegister::SIZE]))
            }
            State::ClearSm(count) => {
                let command = Command::new_write(
                    self.slave_address.into(),
                    SyncManagerControl::ADDRESS + count * 0x08,
                );
                self.buffer.fill(0);
                // disable dl watch dog
                let length = SyncManagerControl::SIZE
                    + SyncManagerStatus::SIZE
                    + SyncManagerActivation::SIZE;
                Some((command, &self.buffer[..length]))
            }
            State::GetVenderID(is_first) => {
                self.inner.into_sii();
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), VenderID::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::GetProductCode(is_first) => {
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), ProductCode::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::GetRevision(is_first) => {
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), RevisionNumber::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::GetProtocol(is_first) => {
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), MailboxProtocol::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::GetRxMailboxSize(is_first) => {
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), StandardRxMailboxSize::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::GetRxMailboxOffset(is_first) => {
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), StandardRxMailboxOffset::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::GetTxMailboxSize(is_first) => {
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), StandardTxMailboxSize::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::GetTxMailboxOffset(is_first) => {
                let sii_reader = self.inner.sii().unwrap();
                if is_first {
                    sii_reader.start(self.slave_address.into(), StandardTxMailboxOffset::ADDRESS);
                }
                sii_reader.next_command(sys_time)
            }
            State::SetSmControl(num) => {
                let command = Command::new_write(
                    self.slave_address.into(),
                    SyncManagerControl::ADDRESS + 0x08 * num as u16,
                );
                self.buffer.fill(0);
                match self.slave_info.as_mut().unwrap().sm[num] {
                    Some(SyncManagerType::MailboxRx(ref sm_info)) => {
                        let mut sm = SyncManagerControl(&mut self.buffer);
                        sm.set_physical_start_address(sm_info.start_address);
                        sm.set_length(sm_info.size);
                        sm.set_buffer_type(0b10); //mailbox
                        sm.set_direction(1); //pdi read access
                        sm.set_dls_user_event_enable(true);
                    }
                    Some(SyncManagerType::MailboxTx(ref sm_info)) => {
                        let mut sm = SyncManagerControl(&mut self.buffer);
                        sm.set_physical_start_address(sm_info.start_address);
                        sm.set_length(sm_info.size);
                        sm.set_buffer_type(0b10); //mailbox
                        sm.set_direction(0); //pdi write access
                        sm.set_dls_user_event_enable(true);
                    }
                    _ => {}
                }
                Some((command, &self.buffer[..SyncManagerControl::SIZE]))
            }
            State::SetSmActivation(num) => {
                let command = Command::new_write(
                    self.slave_address.into(),
                    SyncManagerActivation::ADDRESS + 0x08 * num as u16,
                );
                self.buffer.fill(0);
                match self.slave_info.as_mut().unwrap().sm[num] {
                    Some(SyncManagerType::MailboxRx(_)) | Some(SyncManagerType::MailboxTx(_)) => {
                        let mut sm = SyncManagerActivation(&mut self.buffer);
                        sm.set_channel_enable(true);
                        sm.set_repeat(false);
                    }
                    _ => {}
                }
                Some((command, &self.buffer[..SyncManagerActivation::SIZE]))
            }
            State::SetStationAddress => {
                let command =
                    Command::new_write(self.slave_address.into(), FixedStationAddress::ADDRESS);
                self.buffer.fill(0);
                let mut st_addr = FixedStationAddress(&mut self.buffer);
                let addr = match self.slave_address.into() {
                    SlaveAddress::SlavePosition(addr) => addr + 1,
                    SlaveAddress::StationAddress(addr) => addr,
                };
                self.slave_info.as_mut().unwrap().configured_address = addr;
                st_addr.set_configured_station_address(addr);
                Some((command, &self.buffer[..FixedStationAddress::SIZE]))
            }
            State::CheckPdiControl => {
                // 各種サポート状況の確認
                let command = Command::new_read(self.slave_address.into(), PdiControl::ADDRESS);
                self.buffer.fill(0);
                Some((command, &self.buffer[..PdiControl::SIZE]))
            }
            State::ClearDcActivation => {
                let command = Command::new_write(self.slave_address.into(), DcActivation::ADDRESS);
                self.buffer.fill(0);
                Some((command, &self.buffer[..DcActivation::SIZE]))
            }
            State::ClearCyclicOperationStartTime => {
                let command = Command::new_write(
                    self.slave_address.into(),
                    CyclicOperationStartTime::ADDRESS,
                );
                self.buffer.fill(0);
                Some((command, &self.buffer[..CyclicOperationStartTime::SIZE]))
            }
            State::ClearSync0CycleTime => {
                let command =
                    Command::new_write(self.slave_address.into(), Sync0CycleTime::ADDRESS);
                self.buffer.fill(0);
                Some((command, &self.buffer[..Sync0CycleTime::SIZE]))
            }
            State::ClearSync1CycleTime => {
                let command =
                    Command::new_write(self.slave_address.into(), Sync1CycleTime::ADDRESS);
                self.buffer.fill(0);
                Some((command, &self.buffer[..Sync1CycleTime::SIZE]))
            } //State::ClearLatchEdge => {
              //    let command = Command::new_write(self.slave_address.into(), LatchEdge::ADDRESS);
              //    self.buffer.fill(0);
              //    Some((command, &self.buffer[..LatchEdge::SIZE]))
              //}
              //State::ClearLatchEvent => {
              //    let command = Command::new_write(self.slave_address.into(), LatchEvent::ADDRESS);
              //    self.buffer.fill(0);
              //    Some((command, &self.buffer[..LatchEvent::SIZE]))
              //}
              //State::ClearLatch0PositiveEdgeValue => {
              //    let command =
              //        Command::new_write(self.slave_address.into(), Latch0PositiveEdgeValue::ADDRESS);
              //    self.buffer.fill(0);
              //    Some((command, &self.buffer[..Latch0PositiveEdgeValue::SIZE]))
              //}
              //State::ClearLatch0NegativeEdgeValue => {
              //    let command =
              //        Command::new_write(self.slave_address.into(), Latch0NegativeEdgeValue::ADDRESS);
              //    self.buffer.fill(0);
              //    Some((command, &self.buffer[..Latch0NegativeEdgeValue::SIZE]))
              //}
              //State::ClearLatch1PositiveEdgeValue => {
              //    let command =
              //        Command::new_write(self.slave_address.into(), Latch1PositiveEdgeValue::ADDRESS);
              //    self.buffer.fill(0);
              //    Some((command, &self.buffer[..Latch1PositiveEdgeValue::SIZE]))
              //}
              //State::ClearLatch1NegativeEdgeValue => {
              //    let command =
              //        Command::new_write(self.slave_address.into(), Latch1NegativeEdgeValue::ADDRESS);
              //    self.buffer.fill(0);
              //    Some((command, &self.buffer[..Latch1NegativeEdgeValue::SIZE]))
              //}
        };
        if let Some((command, _)) = command_and_data {
            self.command = command;
        }
        command_and_data
    }

    fn recieve_and_process(
        &mut self,
        recv_data: Option<ReceivedData>,
        sys_time: EtherCatSystemTime,
    ) {
        log::info!("recv {:?}", self.state);
        let data = if let Some(ref recv_data) = recv_data {
            let ReceivedData { command, data, wkc } = recv_data;
            if !(command.c_type == self.command.c_type && command.ado == self.command.ado) {
                self.state = State::Error(EcError::UnexpectedCommand);
            }
            if *wkc != 1 {
                self.state = State::Error(EcError::UnexpectedWkc(*wkc));
            }
            data
        } else {
            self.state = State::Error(EcError::LostPacket);
            return;
        };

        match self.state {
            State::Error(_) => {}
            State::Idle => {}
            State::Complete => {}
            State::SetLoopPort => {
                self.state = State::RequestInitState(true);
            }
            State::RequestInitState(_) => {
                let al_transfer = self.inner.al_state_transfer().unwrap();
                al_transfer.recieve_and_process(recv_data, sys_time);
                match al_transfer.wait() {
                    Some(Ok(AlState::Init)) => {
                        self.slave_info.as_mut().unwrap().al_state = AlState::Init;
                        self.state = State::ResetErrorCount;
                    }
                    Some(Ok(_)) => unreachable!(),
                    None => self.state = State::RequestInitState(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::ResetErrorCount => self.state = State::SetWatchDogDivider,
            State::SetWatchDogDivider => self.state = State::DisableDlWatchDog,
            State::DisableDlWatchDog => self.state = State::DisableSmWatchDog,
            State::DisableSmWatchDog => self.state = State::CheckDlStatus,
            State::CheckDlStatus => {
                let dl_status = DlStatus(data);
                if !dl_status.pdi_operational() {
                    self.state = State::Error(SlaveInitializerError::FailedToLoadEEPROM.into());
                } else {
                    let slave = self.slave_info.as_mut().unwrap();
                    slave.linked_ports[0] = dl_status.signal_detection_port0();
                    slave.linked_ports[1] = dl_status.signal_detection_port1();
                    slave.linked_ports[2] = dl_status.signal_detection_port2();
                    slave.linked_ports[3] = dl_status.signal_detection_port3();
                    self.state = State::CheckDlInfo;
                }
            }
            State::CheckDlInfo => {
                let dl_info = DlInformation(data);
                let slave = self.slave_info.as_mut().unwrap();
                slave.ports[0] = dl_info.port0_type();
                slave.ports[1] = dl_info.port1_type();
                slave.ports[2] = dl_info.port2_type();
                slave.ports[3] = dl_info.port3_type();

                slave.support_dc = dl_info.dc_supported();
                slave.is_dc_range_64bits = dl_info.dc_range();
                slave.support_fmmu_bit_operation = !dl_info.fmmu_bit_operation_not_supported();
                slave.support_lrw = !dl_info.not_lrw_supported(); //これが無いと事実上プロセスデータに対応しない。
                slave.support_rw = !dl_info.not_bafrw_supported(); //これが無いと事実上Dcに対応しない。
                slave.ram_size_kb = dl_info.ram_size();
                //fmmuの確認
                let number_of_fmmu = dl_info.number_of_supported_fmmu_entities();
                slave.number_of_fmmu = number_of_fmmu;
                slave.number_of_sm = dl_info.number_of_supported_sm_channels();
                self.state = State::ClearFmmu(0);
            }
            State::ClearFmmu(count) => {
                if count < 1 {
                    self.state = State::ClearFmmu(count + 1);
                } else {
                    self.state = State::ClearSm(0);
                }
            }
            State::ClearSm(count) => {
                if count < 4 {
                    self.state = State::ClearSm(count + 1);
                } else {
                    self.state = State::GetVenderID(true);
                }
            }
            State::GetVenderID(_) => {
                let sii_reader = self.inner.sii().unwrap();
                sii_reader.recieve_and_process(recv_data, sys_time);
                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        self.slave_info.as_mut().unwrap().id.vender_id =
                            data.sii_data() as u16;
                        self.state = State::GetProductCode(true);
                    }
                    None => self.state = State::GetVenderID(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::GetProductCode(_) => {
                let sii_reader = self.inner.sii().unwrap();
                sii_reader.recieve_and_process(recv_data, sys_time);
                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        self.slave_info.as_mut().unwrap().id.product_code =
                            data.sii_data() as u16;
                        self.state = State::GetRevision(true);
                    }
                    None => self.state = State::GetProductCode(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::GetRevision(_) => {
                let sii_reader = self.inner.sii().unwrap();
                sii_reader.recieve_and_process(recv_data, sys_time);
                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        self.slave_info.as_mut().unwrap().id.revision_number =
                            data.sii_data() as u16;
                        self.state = State::GetProtocol(true);
                    }
                    None => self.state = State::GetRevision(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::GetProtocol(_) => {
                let sii_reader = self.inner.sii().unwrap();
                sii_reader.recieve_and_process(recv_data, sys_time);
                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        self.slave_info.as_mut().unwrap().support_coe = data.0[0].get_bit(2);
                        self.state = State::GetRxMailboxSize(true)
                    }
                    None => self.state = State::GetProtocol(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::GetRxMailboxSize(_) => {
                let sii_reader = self.inner.sii().unwrap();
                sii_reader.recieve_and_process(recv_data, sys_time);
                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        if self.slave_info.as_ref().unwrap().number_of_sm >= 4
                            && data.sii_data() != 0
                        {
                            self.slave_info.as_mut().unwrap().sm[0] =
                                Some(SyncManagerType::MailboxRx(SyncManager {
                                    number:0,
                                    size: data.sii_data() as u16,
                                    start_address: 0,
                                }));
                            self.slave_info.as_mut().unwrap().sm[2] =
                                Some(SyncManagerType::ProcessDataRx);
                        } else if self.slave_info.as_ref().unwrap().number_of_sm >= 2 {
                            self.slave_info.as_mut().unwrap().sm[0] =
                                Some(SyncManagerType::ProcessDataRx);
                        }
                        self.state = State::GetRxMailboxOffset(true);
                    }
                    None => self.state = State::GetRxMailboxSize(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::GetRxMailboxOffset(_) => {
                let sii_reader = self.inner.sii().unwrap();
                //sii_reader.recieve_and_process(recv_data, desc, sys_time);
                sii_reader.recieve_and_process(recv_data, sys_time);                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        match self.slave_info.as_mut().unwrap().sm[0] {
                            Some(SyncManagerType::MailboxRx(ref mut sm)) => {
                                sm.start_address = data.sii_data() as u16
                            }
                            _ => {}
                        }

                        self.state = State::GetTxMailboxSize(true)
                    }
                    None => self.state = State::GetRxMailboxOffset(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::GetTxMailboxSize(_) => {
                let sii_reader = self.inner.sii().unwrap();
                //sii_reader.recieve_and_process(recv_data, desc, sys_time);
                sii_reader.recieve_and_process(recv_data, sys_time);                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        if self.slave_info.as_ref().unwrap().number_of_sm >= 4
                            && data.sii_data() != 0
                        {
                            self.slave_info.as_mut().unwrap().sm[1] =
                                Some(SyncManagerType::MailboxTx(SyncManager {
                                    number:1,
                                    size: data.sii_data() as u16,
                                    start_address: 0,
                                }));
                        } else if self.slave_info.as_ref().unwrap().number_of_sm >= 4 {
                            self.slave_info.as_mut().unwrap().sm[3] =
                                Some(SyncManagerType::ProcessDataTx);
                        }
                        self.state = State::GetTxMailboxOffset(true);
                    }
                    None => self.state = State::GetTxMailboxSize(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::GetTxMailboxOffset(_) => {
                let sii_reader = self.inner.sii().unwrap();
                //sii_reader.recieve_and_process(recv_data, desc, sys_time);
                sii_reader.recieve_and_process(recv_data, sys_time);                match sii_reader.wait() {
                    Some(Ok((data, _size))) => {
                        match self.slave_info.as_mut().unwrap().sm[1] {
                            Some(SyncManagerType::MailboxTx(ref mut sm)) => {
                                sm.start_address = data.sii_data() as u16
                            }
                            _ => {}
                        }
                        set_process_data_sm_size_offset(
                            self.slave_info.as_mut().unwrap(),
                        );

                        self.state = State::SetSmControl(0);
                    }
                    None => self.state = State::GetTxMailboxOffset(false),
                    Some(Err(err)) => {
                        self.state = State::Error(err.into());
                    }
                }
            }
            State::SetSmControl(num) => self.state = State::SetSmActivation(num),
            State::SetSmActivation(num) => {
                if 3 <= num{
                    self.state = State::SetStationAddress;
                }else{
                    self.state = State::SetSmControl(num+1);
                }
            }
            State::SetStationAddress => self.state = State::CheckPdiControl,
            State::CheckPdiControl => {
                let pdi_control = PdiControl(data);
                let slave = self.slave_info.as_mut().unwrap();
                slave.strict_al_control = pdi_control.strict_al_control();
                if slave.support_dc{
                    self.state = State::ClearDcActivation;
                }else{
                    self.state = State::Complete;
                }
            }
            State::ClearDcActivation => self.state = State::ClearCyclicOperationStartTime,
            State::ClearCyclicOperationStartTime => self.state = State::ClearSync0CycleTime,
            State::ClearSync0CycleTime => self.state = State::ClearSync1CycleTime,
            State::ClearSync1CycleTime => {
                self.state = State::Complete;
            }
            //State::ClearLatchEdge => self.state = State::ClearLatchEvent,
            //State::ClearLatchEvent => self.state = State::ClearLatch0PositiveEdgeValue,
            //State::ClearLatch0PositiveEdgeValue => self.state = State::ClearLatch0NegativeEdgeValue,
            //State::ClearLatch0NegativeEdgeValue => self.state = State::ClearLatch1PositiveEdgeValue,
            //State::ClearLatch1PositiveEdgeValue => self.state = State::ClearLatch1NegativeEdgeValue,
            //State::ClearLatch1NegativeEdgeValue => self.state = State::Complete,
        }
    }
}

const fn buffer_size() -> usize {
    let mut size = 0;
    size = const_max(size, DlControl::SIZE);
    size = const_max(size, RxErrorCounter::SIZE);
    size = const_max(size, WatchDogDivider::SIZE);
    size = const_max(size, DlUserWatchDog::SIZE);
    size = const_max(size, SyncManagerChannelWatchDog::SIZE);
    size = const_max(size, DlStatus::SIZE);
    size = const_max(size, DlInformation::SIZE);
    size = const_max(size, FmmuRegister::SIZE);
    size = const_max(
        size,
        SyncManagerControl::SIZE + SyncManagerStatus::SIZE + SyncManagerActivation::SIZE,
    );
    size = const_max(size, FixedStationAddress::SIZE);
    size = const_max(size, PdiControl::SIZE);
    size = const_max(size, DcActivation::SIZE);
    size = const_max(size, CyclicOperationStartTime::SIZE);
    size = const_max(size, Sync0CycleTime::SIZE);
    size = const_max(size, Sync1CycleTime::SIZE);
    size = const_max(size, LatchEdge::SIZE);
    size = const_max(size, LatchEvent::SIZE);
    size = const_max(size, Latch0PositiveEdgeValue::SIZE);
    size = const_max(size, Latch0NegativeEdgeValue::SIZE);
    size = const_max(size, Latch1PositiveEdgeValue::SIZE);
    size = const_max(size, Latch1NegativeEdgeValue::SIZE);
    size = const_max(size, Latch0NegativeEdgeValue::SIZE);
    size = const_max(size, Latch0NegativeEdgeValue::SIZE);
    size
}

fn set_process_data_sm_size_offset(slave: &mut SlaveInfo) {
    if let (Some(SyncManagerType::MailboxRx(ref sm0)), Some(SyncManagerType::MailboxTx(ref sm1))) =
        (&slave.sm[0], &slave.sm[1])
    {
        let sm_address0 = sm0.start_address;
        let sm_size0 = sm0.size;
        let sm_address1 = sm1.start_address;
        let sm_size1 = sm1.size;
        let sm_start_address = sm_address0.min(sm_address1);
        let size1 = if sm_start_address > 0x1000 {
            sm_start_address - 0x1000
        } else {
            0
        };
        let sm_end_address = (sm_address0 + sm_size0 - 1).max(sm_address1 + sm_size1 - 1);
        let end_address = slave.ram_size_kb as u16 * 0x0400 - 1 + 0x1000;
        let size2 = if end_address > sm_end_address {
            end_address - sm_end_address
        } else {
            0
        };
        if size1 > size2 {
            slave.pdo_start_address = Some(0x1000);
            slave.pdo_ram_size = size1;
        } else {
            slave.pdo_start_address = Some(sm_end_address + 1);
            slave.pdo_ram_size = size2;
        }
    } else {
        slave.pdo_start_address = None;
    }
}

#[derive(Debug)]
enum InnerFunction {
    This,
    Sii(SiiReader),
    AlStateTransfer(AlStateTransfer),
}

impl Default for InnerFunction {
    fn default() -> Self {
        Self::This
    }
}

impl InnerFunction {
    fn into_sii(&mut self) {
        if let Self::Sii(_) = &self {
            return;
        }
        *self = Self::Sii(SiiReader::new());
    }

    fn into_al_state_transfer(&mut self) {
        if let Self::AlStateTransfer(_) = &self {
            return;
        }
        *self = Self::AlStateTransfer(AlStateTransfer::new());
    }

    fn sii(&mut self) -> Option<&mut SiiReader> {
        if let Self::Sii(sii) = self {
            Some(sii)
        } else {
            None
        }
    }

    fn al_state_transfer(&mut self) -> Option<&mut AlStateTransfer> {
        if let Self::AlStateTransfer(al) = self {
            Some(al)
        } else {
            None
        }
    }
}