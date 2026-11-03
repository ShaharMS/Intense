use std::{ops::{Deref,DerefMut},sync::{Arc,Condvar,Mutex}};

pub struct Pool<T> { available:Mutex<Vec<T>>, ready:Condvar }
pub struct Lease<T> { owner:Arc<Pool<T>>, value:Option<T> }
impl<T> Pool<T> {
    pub fn new(values:Vec<T>)->Arc<Self> {
        assert!(!values.is_empty());
        Arc::new(Self {available:Mutex::new(values),ready:Condvar::new()})
    }
    pub fn take(self:&Arc<Self>)->Lease<T> {
        let mut available=self.available.lock().unwrap_or_else(|error|error.into_inner());
        loop {
            if let Some(value)=available.pop() {return Lease {owner:self.clone(),value:Some(value)};}
            available=self.ready.wait(available).unwrap_or_else(|error|error.into_inner());
        }
    }
}
impl<T> Deref for Lease<T> {
    type Target=T;
    fn deref(&self)->&T {self.value.as_ref().unwrap()}
}
impl<T> DerefMut for Lease<T> {
    fn deref_mut(&mut self)->&mut T {self.value.as_mut().unwrap()}
}
impl<T> Drop for Lease<T> {
    fn drop(&mut self) {
        if let Some(value)=self.value.take() {
            self.owner.available.lock().unwrap_or_else(|error|error.into_inner()).push(value);
            self.owner.ready.notify_one();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn buffers_return_and_preserve_their_grown_capacity() {
        let pool=Pool::new(vec![Vec::<u8>::new()]);
        {let mut lease=pool.take();lease.resize(4096,7);}
        let lease=pool.take();assert_eq!(lease.len(),4096);assert!(lease.iter().all(|byte|*byte==7));
    }
    #[test]
    fn admission_waits_until_an_exclusive_buffer_is_returned() {
        let pool=Pool::new(vec![1,2]);let first=pool.take();let second=pool.take();
        let (sender,receiver)=std::sync::mpsc::channel();let other=pool.clone();
        let thread=std::thread::spawn(move||{let lease=other.take();sender.send(*lease).unwrap();});
        assert!(receiver.recv_timeout(std::time::Duration::from_millis(30)).is_err());
        drop(first);assert!(receiver.recv_timeout(std::time::Duration::from_secs(1)).is_ok());
        thread.join().unwrap();drop(second);
    }
    #[test]
    fn error_unwinding_returns_the_buffer_without_poisoning_the_pool() {
        let pool=Pool::new(vec![123]);let other=pool.clone();
        assert!(std::panic::catch_unwind(move||{let _lease=other.take();panic!("decode failed");}).is_err());
        assert_eq!(*pool.take(),123);
    }
}
